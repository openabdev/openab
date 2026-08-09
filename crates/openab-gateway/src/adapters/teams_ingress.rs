use reqwest::Url;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::warn;

pub(super) const DEFAULT_DEDUPE_TTL_SECS: u64 = 10 * 60;
pub(super) const DEFAULT_ROUTE_TTL_SECS: u64 = 60 * 60;
pub(super) const DEFAULT_MAX_ROUTE_ENTRIES: usize = 10_000;
pub(super) const TEAMS_ATTACHMENT_AGGREGATE_MAX_BYTES: u64 = 20 * 1024 * 1024;

const PUBLISHING_STALE_TTL: Duration = Duration::from_secs(30);
const PUBLISH_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct TeamsRouteKey {
    app_id: String,
    tenant_id: String,
    conversation_id: String,
    activity_id: String,
}

impl TeamsRouteKey {
    pub(super) fn new(
        app_id: impl Into<String>,
        tenant_id: impl Into<String>,
        conversation_id: impl Into<String>,
        activity_id: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            tenant_id: tenant_id.into(),
            conversation_id: conversation_id.into(),
            activity_id: activity_id.into(),
        }
    }

    fn with_activity_id(&self, activity_id: impl Into<String>) -> Self {
        Self {
            app_id: self.app_id.clone(),
            tenant_id: self.tenant_id.clone(),
            conversation_id: self.conversation_id.clone(),
            activity_id: activity_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TeamsAttachmentSourceKind {
    InlineImage,
    PersonalFileImage,
    PersonalTextFile,
}

#[derive(Clone)]
pub(super) struct TeamsAttachmentSource {
    pub(super) kind: TeamsAttachmentSourceKind,
    pub(super) url: Url,
    pub(super) service_origin: Url,
    pub(super) attachment_type: String,
    pub(super) filename: String,
    pub(super) mime_type: String,
    pub(super) max_bytes: u64,
}

pub(super) struct ClaimedTeamsAttachment {
    pub(super) source: TeamsAttachmentSource,
    pub(super) reserved_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttachmentLookupError {
    RouteNotFound,
    ConversationMismatch,
    ReferenceNotFound,
    AggregateLimitExceeded,
}

/// Gateway-local routing material for one authenticated Teams activity.
///
/// The service URL is intentionally kept out of the wire schema and logging.
/// Outbound Teams sends consume this route by `event_id`; ingress owns its
/// validation, bounds, expiry, and duplicate-safe publication.
#[allow(dead_code)] // authenticated scope fields are retained for later typed routing/ownership
#[derive(Clone)]
pub(super) struct TeamsIngressRoute {
    pub(super) key: TeamsRouteKey,
    pub(super) event_id: String,
    pub(super) tenant_id: String,
    pub(super) conversation_id: String,
    pub(super) conversation_type: String,
    pub(super) inbound_activity_id: String,
    pub(super) reply_chain_root_id: Option<String>,
    pub(super) service_url: Url,
    pub(super) team_id: Option<String>,
    pub(super) channel_id: Option<String>,
    pub(super) attachment_sources: HashMap<String, TeamsAttachmentSource>,
    pub(super) attachment_materialized_bytes: u64,
    pub(super) created_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublishState {
    Publishing,
    Accepted,
    Failed,
}

struct DedupeEntry {
    state: PublishState,
    event_id: String,
    updated_at: Instant,
    completion: watch::Sender<PublishState>,
}

struct OwnedActivityEntry {
    route: TeamsIngressRoute,
    created_at: Instant,
}

pub(super) enum PublishReservation {
    Owner,
    AcceptedDuplicate,
    PublishingDuplicate(watch::Receiver<PublishState>),
    AtCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RouteLookupError {
    NotFound,
    ConversationMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OwnershipLookupError {
    NotOwned,
    OriginRouteNotFound,
    ConversationMismatch,
    AmbiguousScope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReactionLookupError {
    TargetNotKnown,
    OriginRouteNotFound,
    ConversationMismatch,
    AmbiguousScope,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct TeamsIngressCleanupStats {
    pub(crate) routes_removed: usize,
    pub(crate) dedupe_entries_removed: usize,
    pub(crate) stale_publications_removed: usize,
    pub(crate) owned_activities_removed: usize,
}

/// Process-local, bounded Teams route, dedupe, and bot-owned activity state.
///
/// This is deliberately not a durable queue and does not provide cross-replica
/// idempotency. A per-key Publishing state plus completion channel ensures
/// concurrent retries observe the owner's local enqueue result. The ownership
/// index allows edit/delete only for activities created by this process.
pub(super) struct TeamsIngressRegistry {
    routes_by_event: HashMap<String, TeamsIngressRoute>,
    event_by_key: HashMap<TeamsRouteKey, String>,
    dedupe: HashMap<TeamsRouteKey, DedupeEntry>,
    owned: HashMap<TeamsRouteKey, OwnedActivityEntry>,
    dedupe_ttl: Duration,
    route_ttl: Duration,
    max_entries: usize,
}

impl TeamsIngressRegistry {
    pub(super) fn new(dedupe_ttl: Duration, route_ttl: Duration, max_entries: usize) -> Self {
        Self {
            routes_by_event: HashMap::new(),
            event_by_key: HashMap::new(),
            dedupe: HashMap::new(),
            owned: HashMap::new(),
            dedupe_ttl,
            route_ttl,
            max_entries: max_entries.max(1),
        }
    }

    pub(super) fn reserve(
        &mut self,
        key: TeamsRouteKey,
        event_id: String,
        now: Instant,
    ) -> PublishReservation {
        self.cleanup(now);

        if let Some(entry) = self.dedupe.get(&key) {
            return match entry.state {
                PublishState::Accepted => PublishReservation::AcceptedDuplicate,
                PublishState::Publishing => {
                    PublishReservation::PublishingDuplicate(entry.completion.subscribe())
                }
                PublishState::Failed => {
                    // Failed entries are normally removed immediately. Treat a
                    // defensive leftover as vacant instead of suppressing retry.
                    self.dedupe.remove(&key);
                    self.reserve(key, event_id, now)
                }
            };
        }

        if self.dedupe.len() >= self.max_entries && !self.evict_oldest_accepted_dedupe() {
            return PublishReservation::AtCapacity;
        }

        let (completion, _) = watch::channel(PublishState::Publishing);
        self.dedupe.insert(
            key,
            DedupeEntry {
                state: PublishState::Publishing,
                event_id,
                updated_at: now,
                completion,
            },
        );
        PublishReservation::Owner
    }

    pub(super) fn accept(
        &mut self,
        key: &TeamsRouteKey,
        event_id: &str,
        route: TeamsIngressRoute,
        now: Instant,
    ) -> bool {
        let Some(entry) = self.dedupe.get_mut(key) else {
            return false;
        };
        if entry.state != PublishState::Publishing || entry.event_id != event_id {
            return false;
        }

        entry.state = PublishState::Accepted;
        entry.updated_at = now;
        entry.completion.send_replace(PublishState::Accepted);
        self.insert_route(route);
        true
    }

    pub(super) fn fail(&mut self, key: &TeamsRouteKey, event_id: &str) {
        let matches_owner = self
            .dedupe
            .get(key)
            .is_some_and(|entry| entry.event_id == event_id);
        if !matches_owner {
            return;
        }
        if let Some(entry) = self.dedupe.remove(key) {
            entry.completion.send_replace(PublishState::Failed);
            self.remove_route(event_id);
        }
    }

    pub(super) fn cleanup(&mut self, now: Instant) -> TeamsIngressCleanupStats {
        let expired_route_ids: Vec<String> = self
            .routes_by_event
            .iter()
            .filter(|(_, route)| now.saturating_duration_since(route.created_at) >= self.route_ttl)
            .map(|(event_id, _)| event_id.clone())
            .collect();
        for event_id in &expired_route_ids {
            self.remove_route(event_id);
        }

        let expired_dedupe_keys: Vec<TeamsRouteKey> = self
            .dedupe
            .iter()
            .filter(|(_, entry)| match entry.state {
                PublishState::Accepted => {
                    now.saturating_duration_since(entry.updated_at) >= self.dedupe_ttl
                }
                PublishState::Publishing => {
                    now.saturating_duration_since(entry.updated_at) >= PUBLISHING_STALE_TTL
                }
                PublishState::Failed => true,
            })
            .map(|(key, _)| key.clone())
            .collect();

        let mut stale_publications_removed = 0;
        for key in &expired_dedupe_keys {
            if let Some(entry) = self.dedupe.remove(key) {
                if entry.state == PublishState::Publishing {
                    stale_publications_removed += 1;
                    entry.completion.send_replace(PublishState::Failed);
                }
            }
        }

        let expired_owned_keys: Vec<TeamsRouteKey> = self
            .owned
            .iter()
            .filter(|(_, entry)| now.saturating_duration_since(entry.created_at) >= self.route_ttl)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &expired_owned_keys {
            self.owned.remove(key);
        }

        TeamsIngressCleanupStats {
            routes_removed: expired_route_ids.len(),
            dedupe_entries_removed: expired_dedupe_keys.len(),
            stale_publications_removed,
            owned_activities_removed: expired_owned_keys.len(),
        }
    }

    pub(super) fn route_for_reply(
        &mut self,
        event_id: &str,
        conversation_id: &str,
        requested_quote: Option<&str>,
        now: Instant,
    ) -> Result<(TeamsIngressRoute, Option<String>), RouteLookupError> {
        self.cleanup(now);
        let route = self
            .routes_by_event
            .get(event_id)
            .cloned()
            .ok_or(RouteLookupError::NotFound)?;
        if route.conversation_id != conversation_id {
            return Err(RouteLookupError::ConversationMismatch);
        }

        // A quote is safe only when its activity was authenticated in the same
        // app/tenant/conversation scope. The current activity and its declared
        // reply-chain root are already authenticated routing material; older
        // activities must still exist in the bounded route index.
        let quote_activity_id = requested_quote
            .filter(|activity_id| !activity_id.trim().is_empty())
            .filter(|activity_id| {
                route.inbound_activity_id == **activity_id
                    || route.reply_chain_root_id.as_deref() == Some(*activity_id)
                    || self
                        .event_by_key
                        .get(&route.key.with_activity_id(*activity_id))
                        .and_then(|known_event_id| self.routes_by_event.get(known_event_id))
                        .is_some()
            })
            .map(str::to_owned);

        Ok((route, quote_activity_id))
    }

    pub(super) fn claim_attachment(
        &mut self,
        event_id: &str,
        conversation_id: &str,
        reference: &str,
        now: Instant,
    ) -> Result<ClaimedTeamsAttachment, AttachmentLookupError> {
        self.cleanup(now);
        let route = self
            .routes_by_event
            .get_mut(event_id)
            .ok_or(AttachmentLookupError::RouteNotFound)?;
        if route.conversation_id != conversation_id {
            return Err(AttachmentLookupError::ConversationMismatch);
        }

        let remaining = TEAMS_ATTACHMENT_AGGREGATE_MAX_BYTES
            .saturating_sub(route.attachment_materialized_bytes);
        if remaining == 0 {
            return Err(AttachmentLookupError::AggregateLimitExceeded);
        }
        let source = route
            .attachment_sources
            .remove(reference)
            .ok_or(AttachmentLookupError::ReferenceNotFound)?;
        let reserved_bytes = source.max_bytes.min(remaining);
        if reserved_bytes == 0 {
            return Err(AttachmentLookupError::AggregateLimitExceeded);
        }
        route.attachment_materialized_bytes = route
            .attachment_materialized_bytes
            .saturating_add(reserved_bytes);
        Ok(ClaimedTeamsAttachment {
            source,
            reserved_bytes,
        })
    }

    pub(super) fn finish_attachment(
        &mut self,
        event_id: &str,
        reserved_bytes: u64,
        materialized_bytes: u64,
    ) {
        let Some(route) = self.routes_by_event.get_mut(event_id) else {
            return;
        };
        route.attachment_materialized_bytes = route
            .attachment_materialized_bytes
            .saturating_sub(reserved_bytes)
            .saturating_add(materialized_bytes.min(reserved_bytes));
    }

    pub(super) fn route_for_reaction_target(
        &mut self,
        app_id: &str,
        origin_event_id: Option<&str>,
        conversation_id: &str,
        activity_id: &str,
        now: Instant,
    ) -> Result<TeamsIngressRoute, ReactionLookupError> {
        self.cleanup(now);

        if let Some(origin_event_id) = origin_event_id {
            if origin_event_id.is_empty() {
                return Err(ReactionLookupError::OriginRouteNotFound);
            }
            let origin_route = self
                .routes_by_event
                .get(origin_event_id)
                .cloned()
                .ok_or(ReactionLookupError::OriginRouteNotFound)?;
            if origin_route.conversation_id != conversation_id {
                return Err(ReactionLookupError::ConversationMismatch);
            }
            if origin_route.key.app_id != app_id {
                return Err(ReactionLookupError::TargetNotKnown);
            }
            if origin_route.inbound_activity_id == activity_id
                || origin_route.reply_chain_root_id.as_deref() == Some(activity_id)
            {
                return Ok(origin_route);
            }

            let target_key = origin_route.key.with_activity_id(activity_id);
            if let Some(route) = self
                .event_by_key
                .get(&target_key)
                .and_then(|event_id| self.routes_by_event.get(event_id))
            {
                return Ok(route.clone());
            }
            return self
                .owned
                .get(&target_key)
                .map(|entry| entry.route.clone())
                .ok_or(ReactionLookupError::TargetNotKnown);
        }

        let mut candidates = HashMap::<TeamsRouteKey, TeamsIngressRoute>::new();
        for route in self.routes_by_event.values().filter(|route| {
            route.key.app_id == app_id
                && route.conversation_id == conversation_id
                && route.inbound_activity_id == activity_id
        }) {
            candidates.insert(route.key.clone(), route.clone());
        }
        for (key, entry) in self.owned.iter().filter(|(key, _)| {
            key.app_id == app_id
                && key.conversation_id == conversation_id
                && key.activity_id == activity_id
        }) {
            candidates.insert(key.clone(), entry.route.clone());
        }

        let mut candidates = candidates.into_values();
        let route = candidates
            .next()
            .ok_or(ReactionLookupError::TargetNotKnown)?;
        if candidates.next().is_some() {
            return Err(ReactionLookupError::AmbiguousScope);
        }
        Ok(route)
    }

    pub(super) fn record_owned(
        &mut self,
        route: &TeamsIngressRoute,
        activity_id: &str,
        now: Instant,
    ) {
        self.cleanup(now);
        let key = route.key.with_activity_id(activity_id);
        if !self.owned.contains_key(&key) && self.owned.len() >= self.max_entries {
            if let Some(oldest_key) = self
                .owned
                .iter()
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(key, _)| key.clone())
            {
                self.owned.remove(&oldest_key);
                warn!(
                    max_entries = self.max_entries,
                    "teams outbound ownership cache evicted its oldest entry at capacity"
                );
            }
        }
        // Ownership needs the authenticated Connector route but never the
        // presigned attachment URLs. Do not duplicate attachment capabilities
        // into every bot-owned activity entry.
        let mut owned_route = route.clone();
        owned_route.attachment_sources.clear();
        owned_route.attachment_materialized_bytes = 0;
        self.owned.insert(
            key,
            OwnedActivityEntry {
                route: owned_route,
                created_at: now,
            },
        );
    }

    pub(super) fn owned_route_for_target(
        &mut self,
        app_id: &str,
        origin_event_id: Option<&str>,
        conversation_id: &str,
        activity_id: &str,
        now: Instant,
    ) -> Result<TeamsIngressRoute, OwnershipLookupError> {
        self.cleanup(now);

        if let Some(origin_event_id) = origin_event_id {
            if origin_event_id.is_empty() {
                return Err(OwnershipLookupError::OriginRouteNotFound);
            }
            let Some(origin_route) = self.routes_by_event.get(origin_event_id) else {
                return Err(OwnershipLookupError::OriginRouteNotFound);
            };
            if origin_route.conversation_id != conversation_id {
                return Err(OwnershipLookupError::ConversationMismatch);
            }
            return self
                .owned
                .get(&origin_route.key.with_activity_id(activity_id))
                .map(|entry| entry.route.clone())
                .ok_or(OwnershipLookupError::NotOwned);
        }

        let mut candidates = self.owned.iter().filter(|(key, _)| {
            key.app_id == app_id
                && key.conversation_id == conversation_id
                && key.activity_id == activity_id
        });
        let Some((_, candidate)) = candidates.next() else {
            return Err(OwnershipLookupError::NotOwned);
        };
        if candidates.next().is_some() {
            return Err(OwnershipLookupError::AmbiguousScope);
        }
        Ok(candidate.route.clone())
    }

    pub(super) fn remove_owned(&mut self, route: &TeamsIngressRoute, activity_id: &str) -> bool {
        self.owned
            .remove(&route.key.with_activity_id(activity_id))
            .is_some()
    }

    #[cfg(test)]
    pub(super) fn route_for_event(
        &mut self,
        event_id: &str,
        now: Instant,
    ) -> Option<TeamsIngressRoute> {
        self.cleanup(now);
        self.routes_by_event.get(event_id).cloned()
    }

    #[cfg(test)]
    pub(super) fn contains_dedupe_key(&self, key: &TeamsRouteKey) -> bool {
        self.dedupe.contains_key(key)
    }

    fn insert_route(&mut self, route: TeamsIngressRoute) {
        if let Some(previous_event_id) = self.event_by_key.remove(&route.key) {
            self.routes_by_event.remove(&previous_event_id);
        }

        if self.routes_by_event.len() >= self.max_entries {
            if let Some(oldest_event_id) = self
                .routes_by_event
                .iter()
                .min_by_key(|(_, existing)| existing.created_at)
                .map(|(event_id, _)| event_id.clone())
            {
                self.remove_route(&oldest_event_id);
                warn!(
                    max_entries = self.max_entries,
                    "teams ingress route cache evicted its oldest entry at capacity"
                );
            }
        }

        self.event_by_key
            .insert(route.key.clone(), route.event_id.clone());
        self.routes_by_event.insert(route.event_id.clone(), route);
    }

    fn remove_route(&mut self, event_id: &str) {
        if let Some(route) = self.routes_by_event.remove(event_id) {
            if self.event_by_key.get(&route.key).map(String::as_str) == Some(event_id) {
                self.event_by_key.remove(&route.key);
            }
        }
    }

    fn evict_oldest_accepted_dedupe(&mut self) -> bool {
        let Some(oldest_key) = self
            .dedupe
            .iter()
            .filter(|(_, entry)| entry.state == PublishState::Accepted)
            .min_by_key(|(_, entry)| entry.updated_at)
            .map(|(key, _)| key.clone())
        else {
            return false;
        };

        self.dedupe.remove(&oldest_key);
        warn!(
            max_entries = self.max_entries,
            "teams ingress dedupe cache evicted its oldest accepted entry at capacity"
        );
        true
    }
}

pub(super) async fn wait_for_publish(
    mut completion: watch::Receiver<PublishState>,
) -> PublishState {
    if *completion.borrow() != PublishState::Publishing {
        return *completion.borrow();
    }

    match tokio::time::timeout(PUBLISH_WAIT_TIMEOUT, completion.changed()).await {
        Ok(Ok(())) => *completion.borrow(),
        Ok(Err(_)) | Err(_) => PublishState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(index: usize) -> TeamsRouteKey {
        TeamsRouteKey::new("app", "tenant", "conversation", format!("activity-{index}"))
    }

    fn attachment_source(max_bytes: u64) -> anyhow::Result<TeamsAttachmentSource> {
        Ok(TeamsAttachmentSource {
            kind: TeamsAttachmentSourceKind::PersonalTextFile,
            url: Url::parse("https://tenant.sharepoint.com/download?opaque=1")?,
            service_origin: Url::parse("https://smba.trafficmanager.net/emea/")?,
            attachment_type: "text_file".into(),
            filename: "notes.txt".into(),
            mime_type: "text/plain; charset=utf-8".into(),
            max_bytes,
        })
    }

    fn route(
        key: TeamsRouteKey,
        event_id: &str,
        created_at: Instant,
    ) -> anyhow::Result<TeamsIngressRoute> {
        Ok(TeamsIngressRoute {
            tenant_id: "tenant".into(),
            conversation_id: "conversation".into(),
            conversation_type: "personal".into(),
            inbound_activity_id: key.activity_id.clone(),
            reply_chain_root_id: None,
            service_url: Url::parse("https://smba.trafficmanager.net/emea/")?,
            team_id: None,
            channel_id: None,
            attachment_sources: HashMap::new(),
            attachment_materialized_bytes: 0,
            key,
            event_id: event_id.into(),
            created_at,
        })
    }

    #[test]
    fn accepted_duplicate_is_suppressed_until_ttl_expires() -> anyhow::Result<()> {
        let base = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(10), Duration::from_secs(60), 10);
        let route_key = key(1);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), base),
            PublishReservation::Owner
        ));
        assert!(registry.accept(
            &route_key,
            "event-1",
            route(route_key.clone(), "event-1", base)?,
            base
        ));
        assert!(matches!(
            registry.reserve(
                route_key.clone(),
                "duplicate-event".into(),
                base + Duration::from_secs(9)
            ),
            PublishReservation::AcceptedDuplicate
        ));
        assert!(matches!(
            registry.reserve(
                route_key,
                "event-after-ttl".into(),
                base + Duration::from_secs(10)
            ),
            PublishReservation::Owner
        ));
        Ok(())
    }

    #[tokio::test]
    async fn publishing_duplicate_observes_the_owner_result() -> anyhow::Result<()> {
        let base = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let route_key = key(1);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), base),
            PublishReservation::Owner
        ));
        let waiter = match registry.reserve(
            route_key.clone(),
            "duplicate-event".into(),
            base + Duration::from_millis(1),
        ) {
            PublishReservation::PublishingDuplicate(waiter) => waiter,
            _ => panic!("duplicate must wait for the publishing owner"),
        };
        assert!(registry.accept(
            &route_key,
            "event-1",
            route(route_key.clone(), "event-1", base)?,
            base + Duration::from_millis(2)
        ));
        assert_eq!(wait_for_publish(waiter).await, PublishState::Accepted);
        Ok(())
    }

    #[tokio::test]
    async fn publish_failure_returns_to_vacant_and_wakes_duplicates() {
        let base = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let route_key = key(1);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), base),
            PublishReservation::Owner
        ));
        let waiter = match registry.reserve(
            route_key.clone(),
            "duplicate-event".into(),
            base + Duration::from_millis(1),
        ) {
            PublishReservation::PublishingDuplicate(waiter) => waiter,
            _ => panic!("duplicate must wait for the publishing owner"),
        };
        registry.fail(&route_key, "event-1");
        assert_eq!(wait_for_publish(waiter).await, PublishState::Failed);
        assert!(!registry.contains_dedupe_key(&route_key));
        assert!(matches!(
            registry.reserve(route_key, "retry-event".into(), base),
            PublishReservation::Owner
        ));
    }

    #[test]
    fn failed_local_enqueue_rolls_back_a_provisional_route() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let route_key = key(1);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), now),
            PublishReservation::Owner
        ));
        assert!(registry.accept(
            &route_key,
            "event-1",
            route(route_key.clone(), "event-1", now)?,
            now
        ));

        registry.fail(&route_key, "event-1");
        assert!(!registry.contains_dedupe_key(&route_key));
        assert!(registry.route_for_event("event-1", now).is_none());
        Ok(())
    }

    #[test]
    fn route_and_dedupe_state_are_bounded_and_expire_independently() -> anyhow::Result<()> {
        let base = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(10), Duration::from_secs(20), 2);

        for index in 0..3 {
            let route_key = key(index);
            let event_id = format!("event-{index}");
            let now = base + Duration::from_secs(index as u64);
            assert!(matches!(
                registry.reserve(route_key.clone(), event_id.clone(), now),
                PublishReservation::Owner
            ));
            assert!(registry.accept(
                &route_key,
                &event_id,
                route(route_key.clone(), &event_id, now)?,
                now
            ));
        }

        assert!(registry.route_for_event("event-0", base).is_none());
        assert!(registry.route_for_event("event-2", base).is_some());
        let stats = registry.cleanup(base + Duration::from_secs(22));
        assert_eq!(stats.routes_removed, 2);
        assert_eq!(stats.dedupe_entries_removed, 2);
        Ok(())
    }

    #[test]
    fn same_activity_id_in_different_tenants_or_conversations_does_not_collide() {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let keys = [
            TeamsRouteKey::new("app", "tenant-1", "conversation-1", "activity"),
            TeamsRouteKey::new("app", "tenant-2", "conversation-1", "activity"),
            TeamsRouteKey::new("app", "tenant-1", "conversation-2", "activity"),
            TeamsRouteKey::new("other-app", "tenant-1", "conversation-1", "activity"),
        ];

        for (index, route_key) in keys.into_iter().enumerate() {
            assert!(matches!(
                registry.reserve(route_key, format!("event-{index}"), now),
                PublishReservation::Owner
            ));
        }
    }

    #[test]
    fn reply_lookup_uses_event_scope_and_validates_quote_activity() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);

        for index in 0..2 {
            let route_key = key(index);
            let event_id = format!("event-{index}");
            assert!(matches!(
                registry.reserve(route_key.clone(), event_id.clone(), now),
                PublishReservation::Owner
            ));
            let mut accepted_route = route(route_key.clone(), &event_id, now)?;
            if index == 1 {
                accepted_route.reply_chain_root_id = Some("root-activity".into());
            }
            assert!(registry.accept(&route_key, &event_id, accepted_route, now));
        }

        for (route_key, event_id, tenant_id, conversation_id) in [
            (
                TeamsRouteKey::new("app", "other-tenant", "conversation", "cross-tenant"),
                "event-cross-tenant",
                "other-tenant",
                "conversation",
            ),
            (
                TeamsRouteKey::new("app", "tenant", "other-conversation", "cross-conversation"),
                "event-cross-conversation",
                "tenant",
                "other-conversation",
            ),
        ] {
            assert!(matches!(
                registry.reserve(route_key.clone(), event_id.into(), now),
                PublishReservation::Owner
            ));
            let mut accepted_route = route(route_key.clone(), event_id, now)?;
            accepted_route.tenant_id = tenant_id.into();
            accepted_route.conversation_id = conversation_id.into();
            assert!(registry.accept(&route_key, event_id, accepted_route, now));
        }

        let (_, current_quote) = registry
            .route_for_reply("event-1", "conversation", Some("activity-1"), now)
            .map_err(|error| anyhow::anyhow!("unexpected route error: {error:?}"))?;
        assert_eq!(current_quote.as_deref(), Some("activity-1"));

        let (_, root_quote) = registry
            .route_for_reply("event-1", "conversation", Some("root-activity"), now)
            .map_err(|error| anyhow::anyhow!("unexpected route error: {error:?}"))?;
        assert_eq!(root_quote.as_deref(), Some("root-activity"));

        let (_, prior_quote) = registry
            .route_for_reply("event-1", "conversation", Some("activity-0"), now)
            .map_err(|error| anyhow::anyhow!("unexpected route error: {error:?}"))?;
        assert_eq!(prior_quote.as_deref(), Some("activity-0"));

        for unknown_target in ["unknown", "cross-tenant", "cross-conversation"] {
            let (_, unknown_quote) = registry
                .route_for_reply("event-1", "conversation", Some(unknown_target), now)
                .map_err(|error| anyhow::anyhow!("unexpected route error: {error:?}"))?;
            assert!(
                unknown_quote.is_none(),
                "quote target {unknown_target} must not cross route scope"
            );
        }
        assert!(matches!(
            registry.route_for_reply("event-1", "other-conversation", None, now),
            Err(RouteLookupError::ConversationMismatch)
        ));
        assert!(matches!(
            registry.route_for_reply("missing-event", "conversation", None, now),
            Err(RouteLookupError::NotFound)
        ));
        Ok(())
    }

    #[test]
    fn reaction_targets_require_authenticated_scope_and_legacy_uniqueness() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let route_key = key(1);
        let mut origin_route = route(route_key.clone(), "event-1", now)?;
        origin_route.reply_chain_root_id = Some("root-1".into());
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), now),
            PublishReservation::Owner
        ));
        assert!(registry.accept(&route_key, "event-1", origin_route.clone(), now));
        registry.record_owned(&origin_route, "bot-1", now);

        for target in ["activity-1", "root-1", "bot-1"] {
            let resolved = registry
                .route_for_reaction_target("app", Some("event-1"), "conversation", target, now)
                .map_err(|error| anyhow::anyhow!("unexpected reaction route error: {error:?}"))?;
            assert_eq!(resolved.tenant_id, "tenant");
        }
        assert!(matches!(
            registry.route_for_reaction_target(
                "app",
                Some("event-1"),
                "conversation",
                "unknown",
                now
            ),
            Err(ReactionLookupError::TargetNotKnown)
        ));
        assert!(matches!(
            registry.route_for_reaction_target(
                "app",
                Some("event-1"),
                "other-conversation",
                "activity-1",
                now
            ),
            Err(ReactionLookupError::ConversationMismatch)
        ));
        assert!(registry
            .route_for_reaction_target("app", None, "conversation", "activity-1", now)
            .is_ok());

        let other_key = TeamsRouteKey::new("app", "other-tenant", "conversation", "activity-1");
        let mut other_route = route(other_key.clone(), "event-2", now)?;
        other_route.tenant_id = "other-tenant".into();
        assert!(matches!(
            registry.reserve(other_key.clone(), "event-2".into(), now),
            PublishReservation::Owner
        ));
        assert!(registry.accept(&other_key, "event-2", other_route, now));
        assert!(matches!(
            registry.route_for_reaction_target("app", None, "conversation", "activity-1", now),
            Err(ReactionLookupError::AmbiguousScope)
        ));
        Ok(())
    }

    #[test]
    fn bot_owned_activity_index_is_bounded_enforced_and_expiring() -> anyhow::Result<()> {
        let base = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(10), 2);
        let route_key = key(1);
        let mut owned_route = route(route_key.clone(), "event-1", base)?;
        owned_route
            .attachment_sources
            .insert("secret-ref".into(), attachment_source(1024)?);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), base),
            PublishReservation::Owner
        ));
        assert!(registry.accept(&route_key, "event-1", owned_route.clone(), base));

        registry.record_owned(&owned_route, "bot-0", base);
        registry.record_owned(&owned_route, "bot-1", base + Duration::from_secs(1));
        registry.record_owned(&owned_route, "bot-2", base + Duration::from_secs(2));
        assert!(registry
            .owned
            .get(&route_key.with_activity_id("bot-1"))
            .is_some_and(|entry| entry.route.attachment_sources.is_empty()));

        assert!(matches!(
            registry.owned_route_for_target(
                "app",
                Some("event-1"),
                "conversation",
                "bot-0",
                base + Duration::from_secs(2)
            ),
            Err(OwnershipLookupError::NotOwned)
        ));
        assert!(registry
            .owned_route_for_target(
                "app",
                Some("event-1"),
                "conversation",
                "bot-1",
                base + Duration::from_secs(2)
            )
            .is_ok());
        assert!(matches!(
            registry.owned_route_for_target(
                "app",
                Some("missing-event"),
                "conversation",
                "bot-1",
                base + Duration::from_secs(2)
            ),
            Err(OwnershipLookupError::OriginRouteNotFound)
        ));
        assert!(matches!(
            registry.owned_route_for_target(
                "app",
                Some("event-1"),
                "conversation",
                "activity-1",
                base + Duration::from_secs(2)
            ),
            Err(OwnershipLookupError::NotOwned)
        ));
        assert!(registry.remove_owned(&owned_route, "bot-1"));
        assert!(!registry.remove_owned(&owned_route, "bot-1"));

        let stats = registry.cleanup(base + Duration::from_secs(12));
        assert_eq!(stats.owned_activities_removed, 1);
        assert!(matches!(
            registry.owned_route_for_target(
                "app",
                None,
                "conversation",
                "bot-2",
                base + Duration::from_secs(12)
            ),
            Err(OwnershipLookupError::NotOwned)
        ));
        Ok(())
    }

    #[test]
    fn legacy_owned_target_rejects_ambiguous_tenant_scope() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);

        for (tenant, inbound, event_id) in [
            ("tenant-1", "inbound-1", "event-1"),
            ("tenant-2", "inbound-2", "event-2"),
        ] {
            let route_key = TeamsRouteKey::new("app", tenant, "conversation", inbound);
            let mut owned_route = route(route_key.clone(), event_id, now)?;
            owned_route.tenant_id = tenant.into();
            assert!(matches!(
                registry.reserve(route_key.clone(), event_id.into(), now),
                PublishReservation::Owner
            ));
            assert!(registry.accept(&route_key, event_id, owned_route.clone(), now));
            registry.record_owned(&owned_route, "bot-shared-id", now);
        }

        assert!(matches!(
            registry.owned_route_for_target("app", None, "conversation", "bot-shared-id", now),
            Err(OwnershipLookupError::AmbiguousScope)
        ));
        let tenant_one = registry
            .owned_route_for_target("app", Some("event-1"), "conversation", "bot-shared-id", now)
            .map_err(|error| anyhow::anyhow!("unexpected ownership error: {error:?}"))?;
        assert_eq!(tenant_one.tenant_id, "tenant-1");
        assert!(matches!(
            registry.owned_route_for_target(
                "app",
                Some("event-1"),
                "other-conversation",
                "bot-shared-id",
                now
            ),
            Err(OwnershipLookupError::ConversationMismatch)
        ));
        Ok(())
    }

    #[test]
    fn attachment_claim_is_route_scoped_single_use_and_budgeted() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 10);
        let route_key = key(1);
        let mut accepted_route = route(route_key.clone(), "event-1", now)?;
        for reference in ["ref-1", "ref-2", "ref-3"] {
            accepted_route
                .attachment_sources
                .insert(reference.into(), attachment_source(10 * 1024 * 1024)?);
        }
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), now),
            PublishReservation::Owner
        ));
        assert!(registry.accept(&route_key, "event-1", accepted_route, now));

        assert!(matches!(
            registry.claim_attachment("event-1", "other", "ref-1", now),
            Err(AttachmentLookupError::ConversationMismatch)
        ));
        let first = registry
            .claim_attachment("event-1", "conversation", "ref-1", now)
            .map_err(|error| anyhow::anyhow!("unexpected attachment error: {error:?}"))?;
        assert_eq!(first.reserved_bytes, 10 * 1024 * 1024);
        assert_eq!(
            first.source.kind,
            TeamsAttachmentSourceKind::PersonalTextFile
        );
        assert!(matches!(
            registry.claim_attachment("event-1", "conversation", "ref-1", now),
            Err(AttachmentLookupError::ReferenceNotFound)
        ));
        registry.finish_attachment("event-1", first.reserved_bytes, first.reserved_bytes);

        let second = registry
            .claim_attachment("event-1", "conversation", "ref-2", now)
            .map_err(|error| anyhow::anyhow!("unexpected attachment error: {error:?}"))?;
        registry.finish_attachment("event-1", second.reserved_bytes, second.reserved_bytes);
        assert!(matches!(
            registry.claim_attachment("event-1", "conversation", "ref-3", now),
            Err(AttachmentLookupError::AggregateLimitExceeded)
        ));
        assert!(matches!(
            registry.claim_attachment("missing", "conversation", "ref-3", now),
            Err(AttachmentLookupError::RouteNotFound)
        ));
        Ok(())
    }

    #[test]
    fn expired_route_cannot_be_used_for_reply() -> anyhow::Result<()> {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(10), 10);
        let route_key = key(1);
        assert!(matches!(
            registry.reserve(route_key.clone(), "event-1".into(), now),
            PublishReservation::Owner
        ));
        assert!(registry.accept(
            &route_key,
            "event-1",
            route(route_key.clone(), "event-1", now)?,
            now
        ));
        assert!(matches!(
            registry.route_for_reply(
                "event-1",
                "conversation",
                None,
                now + Duration::from_secs(10)
            ),
            Err(RouteLookupError::NotFound)
        ));
        Ok(())
    }

    #[test]
    fn capacity_rejects_when_every_dedupe_entry_is_publishing() {
        let now = Instant::now();
        let mut registry =
            TeamsIngressRegistry::new(Duration::from_secs(60), Duration::from_secs(60), 1);
        assert!(matches!(
            registry.reserve(key(1), "event-1".into(), now),
            PublishReservation::Owner
        ));
        assert!(matches!(
            registry.reserve(key(2), "event-2".into(), now),
            PublishReservation::AtCapacity
        ));
    }
}

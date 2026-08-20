use reqwest::Url;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::warn;

pub(super) const DEFAULT_DEDUPE_TTL_SECS: u64 = 10 * 60;
pub(super) const DEFAULT_ROUTE_TTL_SECS: u64 = 60 * 60;
pub(super) const DEFAULT_MAX_ROUTE_ENTRIES: usize = 10_000;

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
}

/// Gateway-local routing material for one authenticated Teams activity.
///
/// The service URL is intentionally kept out of the wire schema and logging.
/// PR 3 consumes this route by `event_id`; PR 2 owns validation, bounds, expiry,
/// and duplicate-safe publication.
#[allow(dead_code)]
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

pub(super) enum PublishReservation {
    Owner,
    AcceptedDuplicate,
    PublishingDuplicate(watch::Receiver<PublishState>),
    AtCapacity,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct TeamsIngressCleanupStats {
    pub(crate) routes_removed: usize,
    pub(crate) dedupe_entries_removed: usize,
    pub(crate) stale_publications_removed: usize,
}

/// Process-local, bounded Teams route and dedupe state.
///
/// This is deliberately not a durable queue and does not provide cross-replica
/// idempotency. A per-key Publishing state plus completion channel ensures
/// concurrent retries observe the owner's local enqueue result.
pub(super) struct TeamsIngressRegistry {
    routes_by_event: HashMap<String, TeamsIngressRoute>,
    event_by_key: HashMap<TeamsRouteKey, String>,
    dedupe: HashMap<TeamsRouteKey, DedupeEntry>,
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

        TeamsIngressCleanupStats {
            routes_removed: expired_route_ids.len(),
            dedupe_entries_removed: expired_dedupe_keys.len(),
            stale_publications_removed,
        }
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

use super::teams_ingress::{
    wait_for_publish, AttachmentLookupError, OwnershipLookupError, PublishReservation,
    PublishState, ReactionLookupError, RouteLookupError, TeamsAttachmentSource,
    TeamsAttachmentSourceKind, TeamsIngressCleanupStats, TeamsIngressRegistry, TeamsIngressRoute,
    TeamsRouteKey, DEFAULT_DEDUPE_TTL_SECS, DEFAULT_MAX_ROUTE_ENTRIES, DEFAULT_ROUTE_TTL_SECS,
};
use crate::schema::*;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

// --- Bot Framework activity types ---

#[allow(dead_code)] // Bot Framework schema fields — needed for future features
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(rename = "type")]
    pub activity_type: String,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub service_url: Option<String>,
    pub channel_id: Option<String>,
    pub from: Option<ChannelAccount>,
    pub recipient: Option<ChannelAccount>,
    pub conversation: Option<ConversationAccount>,
    pub text: Option<String>,
    pub tenant: Option<TenantInfo>,
    pub channel_data: Option<ChannelData>,
    pub reply_to_id: Option<String>,
    #[serde(default)]
    pub entities: Vec<ActivityEntity>,
    #[serde(default)]
    pub attachments: Vec<ActivityAttachment>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccount {
    pub id: Option<String>,
    pub name: Option<String>,
    pub aad_object_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntity {
    #[serde(default, rename = "type")]
    pub entity_type: String,
    pub mentioned: Option<ChannelAccount>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityAttachment {
    #[serde(default)]
    pub content_type: String,
    pub content_url: Option<String>,
    pub name: Option<String>,
    pub content: Option<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAccount {
    pub id: Option<String>,
    pub conversation_type: Option<String>,
    pub is_group: Option<bool>,
    pub tenant_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantInfo {
    pub id: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelData {
    pub tenant: Option<TenantInfo>,
    pub team: Option<ChannelDataEntity>,
    pub channel: Option<ChannelDataEntity>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDataEntity {
    pub id: Option<String>,
}

impl Activity {
    /// Resolve tenant id from any of the locations Teams may put it.
    pub fn resolved_tenant_id(&self) -> Option<&str> {
        self.tenant
            .as_ref()
            .and_then(|t| t.id.as_deref())
            .or_else(|| {
                self.channel_data
                    .as_ref()
                    .and_then(|c| c.tenant.as_ref())
                    .and_then(|t| t.id.as_deref())
            })
            .or_else(|| {
                self.conversation
                    .as_ref()
                    .and_then(|c| c.tenant_id.as_deref())
            })
    }

    fn missing_required_message_field(&self) -> Option<&'static str> {
        let present = |value: Option<&str>| value.is_some_and(|value| !value.trim().is_empty());
        if !present(self.channel_id.as_deref()) {
            return Some("channelId");
        }
        if !present(self.resolved_tenant_id()) {
            return Some("tenant id");
        }
        if !present(
            self.conversation
                .as_ref()
                .and_then(|conversation| conversation.id.as_deref()),
        ) {
            return Some("conversation id");
        }
        if !present(self.id.as_deref()) {
            return Some("activity id");
        }
        if !present(self.from.as_ref().and_then(|sender| sender.id.as_deref())) {
            return Some("sender id");
        }
        if !present(self.service_url.as_deref()) {
            return Some("serviceUrl");
        }
        None
    }

    fn recipient_info(&self) -> Option<RecipientInfo> {
        let recipient = self.recipient.as_ref()?;
        let id = recipient.id.as_deref().filter(|id| !id.trim().is_empty())?;
        Some(RecipientInfo {
            id: id.to_owned(),
            name: recipient.name.clone().unwrap_or_default(),
        })
    }

    fn mention_info(&self) -> (Vec<String>, Vec<MentionInfo>) {
        let mut mention_ids = Vec::new();
        let mut mention_entities = Vec::new();
        for entity in &self.entities {
            if !entity.entity_type.eq_ignore_ascii_case("mention") {
                continue;
            }
            let Some(id) = entity
                .mentioned
                .as_ref()
                .and_then(|mentioned| mentioned.id.as_deref())
                .filter(|id| !id.trim().is_empty())
            else {
                continue;
            };
            if !mention_ids.iter().any(|known| known == id) {
                mention_ids.push(id.to_owned());
            }
            mention_entities.push(MentionInfo {
                id: id.to_owned(),
                text: entity.text.clone().unwrap_or_default(),
            });
        }
        (mention_ids, mention_entities)
    }

    fn gateway_scope(
        &self,
        tenant_id: &str,
        conversation_id: &str,
        conversation_type: &str,
    ) -> GatewayScope {
        let team_id = self
            .channel_data
            .as_ref()
            .and_then(|data| data.team.as_ref())
            .and_then(|team| team.id.clone())
            .filter(|id| !id.trim().is_empty());
        let channel_id = self
            .channel_data
            .as_ref()
            .and_then(|data| data.channel.as_ref())
            .and_then(|channel| channel.id.clone())
            .filter(|id| !id.trim().is_empty());
        let trust_scope_id = match conversation_type {
            "personal" => format!("teams:{tenant_id}:personal:{conversation_id}"),
            "groupChat" => format!("teams:{tenant_id}:group-chat:{conversation_id}"),
            "channel" => match (team_id.as_deref(), channel_id.as_deref()) {
                (Some(team), Some(channel)) => {
                    format!("teams:{tenant_id}:team:{team}:channel:{channel}")
                }
                _ => format!("teams:{tenant_id}:invalid-channel:{conversation_id}"),
            },
            other => format!("teams:{tenant_id}:unknown:{other}:{conversation_id}"),
        };
        GatewayScope {
            tenant_id: Some(tenant_id.to_owned()),
            team_id,
            channel_id,
            conversation_type: conversation_type.to_owned(),
            trust_scope_id,
            is_dm: conversation_type == "personal",
        }
    }
}

fn canonical_conversation_type(value: &str) -> String {
    if value.eq_ignore_ascii_case("personal") {
        "personal".into()
    } else if value.eq_ignore_ascii_case("groupChat") {
        "groupChat".into()
    } else if value.eq_ignore_ascii_case("channel") {
        "channel".into()
    } else {
        value.trim().to_owned()
    }
}

// --- OpenID configuration ---

#[derive(Debug, Deserialize)]
struct OpenIdConfig {
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

#[derive(Debug, Clone, Deserialize)]
struct JwkKey {
    kid: Option<String>,
    n: String,
    e: String,
    kty: String,
    #[serde(default)]
    endorsements: Vec<String>,
}

// --- OAuth token ---

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

#[derive(Clone)]
struct CachedOpenId {
    jwks_uri: reqwest::Url,
    fetched_at: Instant,
}

#[derive(Clone)]
struct CachedJwks {
    keys: Vec<JwkKey>,
    fetched_at: Instant,
}

// --- Teams adapter config ---

pub struct TeamsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub oauth_endpoint: String,
    pub openid_metadata: String,
    pub allowed_tenants: Vec<String>,
    pub dedupe_ttl_secs: u64,
    pub route_ttl_secs: u64,
    pub max_route_entries: usize,
    pub reactions_enabled: bool,
    pub inbound_attachments: bool,
}

impl TeamsConfig {
    pub fn from_env() -> Option<Self> {
        Self::from_reader(|k| std::env::var(k).ok())
    }

    /// Build config from an arbitrary string reader (#1380) — shared by
    /// env-derived construction and `apply_teams_config`, so the same
    /// mandatory-credential semantics apply to both paths.
    pub(crate) fn from_reader<F: Fn(&str) -> Option<String>>(read: F) -> Option<Self> {
        let app_id = read("TEAMS_APP_ID")?;
        let app_secret = read("TEAMS_APP_SECRET")?;
        Some(Self {
            app_id,
            app_secret,
            oauth_endpoint: read("TEAMS_OAUTH_ENDPOINT").unwrap_or_else(|| {
                "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token".into()
            }),
            openid_metadata: read("TEAMS_OPENID_METADATA").unwrap_or_else(|| {
                "https://login.botframework.com/v1/.well-known/openidconfiguration".into()
            }),
            allowed_tenants: read("TEAMS_ALLOWED_TENANTS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            dedupe_ttl_secs: parse_positive_u64(
                read("TEAMS_DEDUPE_TTL_SECS"),
                "TEAMS_DEDUPE_TTL_SECS",
                DEFAULT_DEDUPE_TTL_SECS,
            ),
            route_ttl_secs: parse_positive_u64(
                read("TEAMS_ROUTE_TTL_SECS"),
                "TEAMS_ROUTE_TTL_SECS",
                DEFAULT_ROUTE_TTL_SECS,
            ),
            max_route_entries: parse_positive_usize(
                read("TEAMS_MAX_ROUTE_ENTRIES"),
                "TEAMS_MAX_ROUTE_ENTRIES",
                DEFAULT_MAX_ROUTE_ENTRIES,
            ),
            reactions_enabled: parse_opt_in_bool(
                read("TEAMS_REACTIONS_ENABLED"),
                "TEAMS_REACTIONS_ENABLED",
            ),
            inbound_attachments: parse_opt_in_bool(
                read("TEAMS_INBOUND_ATTACHMENTS"),
                "TEAMS_INBOUND_ATTACHMENTS",
            ),
        })
    }
}

fn parse_opt_in_bool(raw: Option<String>, key: &str) -> bool {
    match raw.as_deref().map(str::trim) {
        None | Some("") | Some("0") => false,
        Some(value) if value.eq_ignore_ascii_case("false") => false,
        Some("1") => true,
        Some(value) if value.eq_ignore_ascii_case("true") => true,
        Some(_) => {
            warn!(key, "invalid opt-in Teams boolean; using false");
            false
        }
    }
}

fn parse_positive_u64(raw: Option<String>, key: &str, default: u64) -> u64 {
    match raw.as_deref().map(str::trim) {
        None | Some("") => default,
        Some(value) => match value.parse::<u64>() {
            Ok(value) if value > 0 => value,
            _ => {
                warn!(
                    key,
                    default, "invalid positive Teams runtime setting; using default"
                );
                default
            }
        },
    }
}

fn parse_positive_usize(raw: Option<String>, key: &str, default: usize) -> usize {
    match raw.as_deref().map(str::trim) {
        None | Some("") => default,
        Some(value) => match value.parse::<usize>() {
            Ok(value) if value > 0 => value,
            _ => {
                warn!(
                    key,
                    default, "invalid positive Teams runtime setting; using default"
                );
                default
            }
        },
    }
}

// --- Teams adapter state ---

pub struct TeamsAdapter {
    config: TeamsConfig,
    client: reqwest::Client,
    attachment_client: reqwest::Client,
    token_cache: RwLock<Option<CachedToken>>,
    token_refresh_lock: Mutex<()>,
    openid_cache: RwLock<Option<CachedOpenId>>,
    openid_refresh_lock: Mutex<()>,
    jwks_cache: RwLock<Option<CachedJwks>>,
    jwks_refresh_lock: Mutex<()>,
    ingress: Mutex<TeamsIngressRegistry>,
    conversation_writes: Vec<Mutex<()>>,
    allow_non_public_endpoints: bool,
}

const AUTH_CACHE_TTL: Duration = Duration::from_secs(3600);
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);
const TEAMS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TEAMS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEAMS_ERROR_BODY_LIMIT: usize = 4 * 1024;
const TEAMS_MAX_REDIRECTS: usize = 5;
const TEAMS_WRITE_SHARDS: usize = 64;
const TEAMS_ATTACHMENT_METADATA_LIMIT: usize = 10;
const TEAMS_IMAGE_DOWNLOAD_LIMIT: u64 = 10 * 1024 * 1024;
const TEAMS_TEXT_DOWNLOAD_LIMIT: u64 = 512 * 1024;
const TEAMS_MATERIALIZED_FRAME_LIMIT: usize = 8 * 1024 * 1024;
const TEAMS_FILENAME_LIMIT: usize = 200;
const TEAMS_MUTATION_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);
const TEAMS_PUBLIC_SERVICE_HOST: &str = "smba.trafficmanager.net";
const TEAMS_PUBLIC_OAUTH_HOST: &str = "login.microsoftonline.com";
const TEAMS_PUBLIC_OPENID_HOST: &str = "login.botframework.com";

#[derive(Clone, Copy)]
enum ConnectorWriteBody<'a> {
    Absent,
    Empty,
    Json(&'a serde_json::Value),
}

impl TeamsAdapter {
    pub fn new(config: TeamsConfig) -> Self {
        Self::with_client(
            config,
            build_http_client(TEAMS_REQUEST_TIMEOUT),
            false,
            TEAMS_REQUEST_TIMEOUT,
        )
    }

    fn with_client(
        config: TeamsConfig,
        client: reqwest::Client,
        allow_non_public_endpoints: bool,
        attachment_timeout: Duration,
    ) -> Self {
        if config.reactions_enabled {
            warn!("teams message reactions are enabled through a Microsoft public-preview API");
        }
        let ingress = TeamsIngressRegistry::new(
            Duration::from_secs(config.dedupe_ttl_secs),
            Duration::from_secs(config.route_ttl_secs),
            config.max_route_entries,
        );
        Self {
            config,
            client,
            attachment_client: build_attachment_http_client(attachment_timeout),
            token_cache: RwLock::new(None),
            token_refresh_lock: Mutex::new(()),
            openid_cache: RwLock::new(None),
            openid_refresh_lock: Mutex::new(()),
            jwks_cache: RwLock::new(None),
            jwks_refresh_lock: Mutex::new(()),
            ingress: Mutex::new(ingress),
            conversation_writes: (0..TEAMS_WRITE_SHARDS).map(|_| Mutex::new(())).collect(),
            allow_non_public_endpoints,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(config: TeamsConfig) -> Self {
        Self::with_client(
            config,
            build_http_client(TEAMS_REQUEST_TIMEOUT),
            true,
            TEAMS_REQUEST_TIMEOUT,
        )
    }

    #[cfg(test)]
    fn new_for_test_with_timeout(config: TeamsConfig, request_timeout: Duration) -> Self {
        Self::with_client(
            config,
            build_http_client(request_timeout),
            true,
            request_timeout,
        )
    }

    #[cfg(test)]
    pub(crate) async fn accept_route_for_test(
        &self,
        service_url: &str,
        event_id: &str,
        tenant_id: &str,
        conversation_id: &str,
        activity_id: &str,
        reply_chain_root_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        let route_key = TeamsRouteKey::new(
            self.config.app_id.clone(),
            tenant_id,
            conversation_id,
            activity_id,
        );
        let route = TeamsIngressRoute {
            key: route_key.clone(),
            event_id: event_id.into(),
            tenant_id: tenant_id.into(),
            conversation_id: conversation_id.into(),
            conversation_type: "personal".into(),
            inbound_activity_id: activity_id.into(),
            reply_chain_root_id: reply_chain_root_id.map(str::to_owned),
            service_url: reqwest::Url::parse(service_url)?,
            team_id: None,
            channel_id: None,
            attachment_sources: HashMap::new(),
            attachment_materialized_bytes: 0,
            created_at: now,
        };
        let mut ingress = self.ingress.lock().await;
        assert!(matches!(
            ingress.reserve(route_key.clone(), event_id.into(), now),
            PublishReservation::Owner
        ));
        assert!(ingress.accept(&route_key, event_id, route, now));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn accept_text_attachment_route_for_test(
        &self,
        service_url: &str,
        event_id: &str,
        conversation_id: &str,
        activity_id: &str,
        reference: &str,
        download_url: &str,
    ) -> anyhow::Result<()> {
        let now = Instant::now();
        let route_key = TeamsRouteKey::new(
            self.config.app_id.clone(),
            "tenant-1",
            conversation_id,
            activity_id,
        );
        let service_origin = reqwest::Url::parse(service_url)?;
        let mut attachment_sources = HashMap::new();
        attachment_sources.insert(
            reference.into(),
            TeamsAttachmentSource {
                kind: TeamsAttachmentSourceKind::PersonalTextFile,
                url: reqwest::Url::parse(download_url)?,
                service_origin: service_origin.clone(),
                attachment_type: "text_file".into(),
                filename: "notes.txt".into(),
                mime_type: "text/plain; charset=utf-8".into(),
                max_bytes: TEAMS_TEXT_DOWNLOAD_LIMIT,
            },
        );
        let route = TeamsIngressRoute {
            key: route_key.clone(),
            event_id: event_id.into(),
            tenant_id: "tenant-1".into(),
            conversation_id: conversation_id.into(),
            conversation_type: "personal".into(),
            inbound_activity_id: activity_id.into(),
            reply_chain_root_id: None,
            service_url: service_origin,
            team_id: None,
            channel_id: None,
            attachment_sources,
            attachment_materialized_bytes: 0,
            created_at: now,
        };
        let mut ingress = self.ingress.lock().await;
        assert!(matches!(
            ingress.reserve(route_key.clone(), event_id.into(), now),
            PublishReservation::Owner
        ));
        assert!(ingress.accept(&route_key, event_id, route, now));
        Ok(())
    }

    pub(crate) async fn cleanup_ingress(&self) -> TeamsIngressCleanupStats {
        self.ingress.lock().await.cleanup(Instant::now())
    }

    pub fn reactions_enabled(&self) -> bool {
        self.config.reactions_enabled
    }

    pub fn inbound_attachments_enabled(&self) -> bool {
        self.config.inbound_attachments
    }

    fn conversation_write_shard(route: &TeamsIngressRoute) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        route.tenant_id.hash(&mut hasher);
        route.conversation_id.hash(&mut hasher);
        (hasher.finish() as usize) % TEAMS_WRITE_SHARDS
    }

    async fn lock_conversation<'a>(
        &'a self,
        route: &TeamsIngressRoute,
    ) -> tokio::sync::MutexGuard<'a, ()> {
        self.conversation_writes[Self::conversation_write_shard(route)]
            .lock()
            .await
    }

    async fn cached_token(&self) -> Option<String> {
        let cache = self.token_cache.read().await;
        cache.as_ref().and_then(|cached| {
            (cached.expires_at > Instant::now() + TOKEN_REFRESH_MARGIN)
                .then(|| cached.token.clone())
        })
    }

    /// Get a valid OAuth bearer token, refreshing once for concurrent callers.
    async fn get_token(&self) -> anyhow::Result<String> {
        if let Some(token) = self.cached_token().await {
            return Ok(token);
        }

        let _refresh_guard = self.token_refresh_lock.lock().await;
        if let Some(token) = self.cached_token().await {
            return Ok(token);
        }

        let endpoint = validate_public_cloud_endpoint(
            &self.config.oauth_endpoint,
            "Teams OAuth endpoint",
            TEAMS_PUBLIC_OAUTH_HOST,
            self.allow_non_public_endpoints,
        )?;
        let response = self
            .client
            .post(endpoint)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.config.app_id),
                ("client_secret", &self.config.app_secret),
                ("scope", "https://api.botframework.com/.default"),
            ])
            .send()
            .await
            .map_err(|error| safe_request_error("Teams OAuth request", &error))?;
        let response = require_http_success(
            response,
            "Teams OAuth request",
            &[self.config.app_secret.as_str()],
        )
        .await?;
        let response: TokenResponse = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Teams OAuth response was not valid JSON"))?;
        if response.access_token.is_empty() {
            anyhow::bail!("Teams OAuth response missing access token");
        }
        let expires_at = Instant::now()
            .checked_add(Duration::from_secs(response.expires_in))
            .ok_or_else(|| anyhow::anyhow!("Teams OAuth expiry is out of range"))?;

        let token = response.access_token.clone();
        *self.token_cache.write().await = Some(CachedToken {
            token: response.access_token,
            expires_at,
        });
        info!("teams OAuth token refreshed");
        Ok(token)
    }

    async fn cached_openid(&self) -> Option<CachedOpenId> {
        let cache = self.openid_cache.read().await;
        cache
            .as_ref()
            .filter(|cached| cached.fetched_at.elapsed() < AUTH_CACHE_TTL)
            .cloned()
    }

    /// Resolve and cache Microsoft's JWKS endpoint, with one metadata request
    /// shared by all concurrent callers after cache expiry.
    async fn get_openid_jwks_uri(&self) -> anyhow::Result<reqwest::Url> {
        if let Some(cached) = self.cached_openid().await {
            return Ok(cached.jwks_uri);
        }

        let _refresh_guard = self.openid_refresh_lock.lock().await;
        if let Some(cached) = self.cached_openid().await {
            return Ok(cached.jwks_uri);
        }

        let endpoint = validate_public_cloud_endpoint(
            &self.config.openid_metadata,
            "Teams OpenID metadata endpoint",
            TEAMS_PUBLIC_OPENID_HOST,
            self.allow_non_public_endpoints,
        )?;
        let response = self
            .client
            .get(endpoint)
            .send()
            .await
            .map_err(|error| safe_request_error("Teams OpenID metadata request", &error))?;
        let response = require_http_success(response, "Teams OpenID metadata request", &[]).await?;
        let config: OpenIdConfig = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Teams OpenID metadata was not valid JSON"))?;
        let jwks_uri = validate_public_cloud_endpoint(
            &config.jwks_uri,
            "Teams JWKS endpoint",
            TEAMS_PUBLIC_OPENID_HOST,
            self.allow_non_public_endpoints,
        )?;

        *self.openid_cache.write().await = Some(CachedOpenId {
            jwks_uri: jwks_uri.clone(),
            fetched_at: Instant::now(),
        });
        Ok(jwks_uri)
    }

    async fn cached_jwks(&self) -> Option<CachedJwks> {
        let cache = self.jwks_cache.read().await;
        cache
            .as_ref()
            .filter(|cached| cached.fetched_at.elapsed() < AUTH_CACHE_TTL)
            .cloned()
    }

    async fn fetch_jwks(&self) -> anyhow::Result<CachedJwks> {
        let endpoint = self.get_openid_jwks_uri().await?;
        let response = self
            .client
            .get(endpoint)
            .send()
            .await
            .map_err(|error| safe_request_error("Teams JWKS request", &error))?;
        let response = require_http_success(response, "Teams JWKS request", &[]).await?;
        let jwks: JwksResponse = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Teams JWKS response was not valid JSON"))?;
        if jwks.keys.is_empty() {
            anyhow::bail!("Teams JWKS response contained no keys");
        }

        let cached = CachedJwks {
            keys: jwks.keys,
            fetched_at: Instant::now(),
        };
        *self.jwks_cache.write().await = Some(cached.clone());
        info!(count = cached.keys.len(), "teams JWKS keys refreshed");
        Ok(cached)
    }

    /// Fetch and cache JWKS signing keys, sharing one refresh among concurrent
    /// webhook callers after cache expiry.
    async fn get_jwks(&self) -> anyhow::Result<CachedJwks> {
        if let Some(cached) = self.cached_jwks().await {
            return Ok(cached);
        }

        let _refresh_guard = self.jwks_refresh_lock.lock().await;
        if let Some(cached) = self.cached_jwks().await {
            return Ok(cached);
        }
        self.fetch_jwks().await
    }

    /// Refresh keys after a `kid` miss. The observed generation prevents a
    /// burst of concurrent misses from issuing sequential duplicate refreshes.
    async fn refresh_jwks(&self, observed_at: Instant) -> anyhow::Result<CachedJwks> {
        let _refresh_guard = self.jwks_refresh_lock.lock().await;
        if let Some(cached) = self.jwks_cache.read().await.as_ref() {
            if cached.fetched_at != observed_at {
                return Ok(cached.clone());
            }
        }
        self.fetch_jwks().await
    }

    /// Validate the JWT bearer token from an inbound Bot Framework request.
    /// Checks: signature, issuer, audience, expiry, serviceUrl claim, and channel endorsements.
    pub async fn validate_jwt(&self, auth_header: &str, activity: &Activity) -> anyhow::Result<()> {
        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| anyhow::anyhow!("missing Bearer prefix"))?;

        // Decode header to get kid
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header
            .kid
            .ok_or_else(|| anyhow::anyhow!("no kid in JWT header"))?;

        let snapshot = self.get_jwks().await?;
        let key = match snapshot
            .keys
            .iter()
            .find(|key| key.kid.as_deref() == Some(&kid))
        {
            Some(key) => key.clone(),
            None => {
                // Cache miss: Microsoft may have rotated keys. Force refresh and retry.
                let refreshed = self.refresh_jwks(snapshot.fetched_at).await?;
                refreshed
                    .keys
                    .into_iter()
                    .find(|key| key.kid.as_deref() == Some(&kid))
                    .ok_or_else(|| anyhow::anyhow!("no matching JWK after refresh"))?
            }
        };

        if key.kty != "RSA" {
            anyhow::bail!("unsupported key type: {}", key.kty);
        }

        // B2: Validate channel endorsements — key must endorse the activity's channelId
        let channel_id = activity
            .channel_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("activity missing channelId"))?;
        if key.endorsements.is_empty() {
            anyhow::bail!("JWK has no endorsements — cannot verify channelId={channel_id}");
        }
        if !key
            .endorsements
            .iter()
            .any(|endorsement| endorsement == channel_id)
        {
            anyhow::bail!("JWK does not endorse activity channelId");
        }

        let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.config.app_id]);
        // Bot Framework tokens can use RS256 or RS384
        validation.algorithms = vec![Algorithm::RS256, Algorithm::RS384];
        // M0 supports the Microsoft commercial public-cloud issuer only.
        validation.set_issuer(&["https://api.botframework.com"]);
        validation.validate_aud = true;
        validation.validate_exp = true;
        validation.validate_nbf = false;

        let token_data = decode::<serde_json::Value>(token, &decoding_key, &validation)?;

        // B1: Validate serviceUrl claim matches activity's serviceUrl without
        // copying either full URL into an error that will be logged.
        let activity_service_url = activity
            .service_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("activity missing serviceUrl"))?;
        let token_service_url = token_data
            .claims
            .get("serviceurl")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::anyhow!("JWT missing serviceurl claim"))?;
        if token_service_url != activity_service_url {
            anyhow::bail!("serviceUrl claim does not match activity");
        }

        Ok(())
    }

    /// Check tenant allowlist.
    fn check_tenant(&self, activity: &Activity) -> bool {
        if self.config.allowed_tenants.is_empty() {
            return true;
        }
        activity
            .resolved_tenant_id()
            .is_some_and(|tenant_id| self.config.allowed_tenants.iter().any(|a| a == tenant_id))
    }

    fn connector_url(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: Option<&str>,
    ) -> anyhow::Result<reqwest::Url> {
        connector_url(
            service_url,
            conversation_id,
            activity_id,
            self.allow_non_public_endpoints,
        )
    }

    fn reaction_url(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        reaction_type: &str,
    ) -> anyhow::Result<reqwest::Url> {
        reaction_url(
            service_url,
            conversation_id,
            activity_id,
            reaction_type,
            self.allow_non_public_endpoints,
        )
    }

    fn validate_service_url(&self, service_url: &str) -> anyhow::Result<reqwest::Url> {
        validate_public_cloud_endpoint(
            service_url,
            "Teams service URL",
            TEAMS_PUBLIC_SERVICE_HOST,
            self.allow_non_public_endpoints,
        )
    }

    /// Send a reply via Bot Framework REST API and preserve whether a failed
    /// POST was rejected or may already have reached Teams.
    pub async fn send_activity_outcome(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
        reply_to_id: Option<&str>,
    ) -> WriteOutcome {
        // Bot Connector distinguishes a plain conversation send from a reply
        // by endpoint. A route-scoped quote must use ReplyToActivity; setting
        // only Activity.replyToId on SendToConversation is not sufficient for
        // Teams clients to render the reply relationship.
        let url = match self.connector_url(service_url, conversation_id, reply_to_id) {
            Ok(url) => url,
            Err(error) => {
                return WriteOutcome::Rejected {
                    code: "invalid_route".into(),
                    message: error.to_string(),
                    retry_after_ms: None,
                };
            }
        };
        let token = match self.get_token().await {
            Ok(token) => token,
            Err(error) => {
                return WriteOutcome::Rejected {
                    code: "connector_auth_failed".into(),
                    message: error.to_string(),
                    retry_after_ms: None,
                };
            }
        };

        let mut body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
            "textFormat": "markdown",
        });
        if let Some(id) = reply_to_id {
            body["replyToId"] = serde_json::Value::String(id.to_string());
        }

        let response = match self
            .client
            .post(url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let code = if error.is_timeout() {
                    "request_timeout"
                } else {
                    "transport_error"
                };
                return WriteOutcome::Unknown {
                    code: code.into(),
                    message: safe_request_error("Bot Framework send", &error).to_string(),
                };
            }
        };

        let status = response.status();
        if status.is_success() {
            let result: serde_json::Value = match response.json().await {
                Ok(result) => result,
                Err(_) => {
                    return WriteOutcome::Unknown {
                        code: "invalid_success_response".into(),
                        message: "Bot Framework send succeeded without a valid JSON response"
                            .into(),
                    };
                }
            };
            return match result
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
            {
                Some(activity_id) => WriteOutcome::Delivered {
                    message_id: Some(activity_id.to_owned()),
                },
                None => WriteOutcome::Unknown {
                    code: "missing_activity_id".into(),
                    message: "Bot Framework send response missing activity id".into(),
                },
            };
        }

        classify_write_failure(response, "Bot Framework send", &[token.as_str()]).await
    }

    /// Compatibility wrapper for callers that predate structured outcomes.
    pub async fn send_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
        reply_to_id: Option<&str>,
    ) -> anyhow::Result<String> {
        match self
            .send_activity_outcome(service_url, conversation_id, text, reply_to_id)
            .await
        {
            WriteOutcome::Delivered {
                message_id: Some(message_id),
            } => Ok(message_id),
            WriteOutcome::Delivered { message_id: None } => {
                anyhow::bail!("Bot Framework send response missing activity id")
            }
            WriteOutcome::Rejected { message, .. } | WriteOutcome::Unknown { message, .. } => {
                Err(anyhow::anyhow!(message))
            }
        }
    }

    async fn idempotent_connector_write_outcome(
        &self,
        method: reqwest::Method,
        url: reqwest::Url,
        body: ConnectorWriteBody<'_>,
        operation: &'static str,
    ) -> WriteOutcome {
        let token = match self.get_token().await {
            Ok(token) => token,
            Err(error) => {
                return WriteOutcome::Rejected {
                    code: "connector_auth_failed".into(),
                    message: error.to_string(),
                    retry_after_ms: None,
                };
            }
        };

        let mut retried_rate_limit = false;
        loop {
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            request = match body {
                ConnectorWriteBody::Absent => request,
                ConnectorWriteBody::Empty => request.header(reqwest::header::CONTENT_LENGTH, "0"),
                ConnectorWriteBody::Json(body) => request.json(body),
            };
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    let code = if error.is_timeout() {
                        "request_timeout"
                    } else {
                        "transport_error"
                    };
                    return WriteOutcome::Unknown {
                        code: code.into(),
                        message: safe_request_error(operation, &error).to_string(),
                    };
                }
            };
            if response.status().is_success() {
                return WriteOutcome::Delivered { message_id: None };
            }

            let outcome = classify_write_failure(response, operation, &[token.as_str()]).await;
            if !retried_rate_limit {
                if let WriteOutcome::Rejected {
                    code,
                    retry_after_ms: Some(delay_ms),
                    ..
                } = &outcome
                {
                    let delay = Duration::from_millis(*delay_ms);
                    if code == "rate_limited" && delay <= TEAMS_MUTATION_RETRY_MAX_DELAY {
                        warn!(
                            operation,
                            retry_after_ms = *delay_ms,
                            "teams: retrying rate-limited Connector write once"
                        );
                        retried_rate_limit = true;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }
            }
            return outcome;
        }
    }

    async fn mutate_activity_outcome(
        &self,
        method: reqwest::Method,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        body: Option<&serde_json::Value>,
        operation: &'static str,
    ) -> WriteOutcome {
        let url = match self.connector_url(service_url, conversation_id, Some(activity_id)) {
            Ok(url) => url,
            Err(error) => {
                return WriteOutcome::Rejected {
                    code: "invalid_route".into(),
                    message: error.to_string(),
                    retry_after_ms: None,
                };
            }
        };
        let body = body.map_or(ConnectorWriteBody::Absent, ConnectorWriteBody::Json);
        self.idempotent_connector_write_outcome(method, url, body, operation)
            .await
    }

    pub async fn update_activity_outcome(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        text: &str,
    ) -> WriteOutcome {
        let body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
            "textFormat": "markdown",
        });
        self.mutate_activity_outcome(
            reqwest::Method::PUT,
            service_url,
            conversation_id,
            activity_id,
            Some(&body),
            "Bot Framework update",
        )
        .await
    }

    pub async fn delete_activity_outcome(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
    ) -> WriteOutcome {
        self.mutate_activity_outcome(
            reqwest::Method::DELETE,
            service_url,
            conversation_id,
            activity_id,
            None,
            "Bot Framework delete",
        )
        .await
    }

    async fn reaction_activity_outcome(
        &self,
        method: reqwest::Method,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        reaction: &str,
        operation: &'static str,
    ) -> WriteOutcome {
        let Some(reaction_type) = teams_reaction_type(reaction) else {
            return WriteOutcome::Rejected {
                code: "unsupported_reaction".into(),
                message: "Teams reaction is not a supported emoji or reaction ID".into(),
                retry_after_ms: None,
            };
        };
        let url = match self.reaction_url(
            service_url,
            conversation_id,
            activity_id,
            reaction_type.as_ref(),
        ) {
            Ok(url) => url,
            Err(error) => {
                return WriteOutcome::Rejected {
                    code: "invalid_route".into(),
                    message: error.to_string(),
                    retry_after_ms: None,
                };
            }
        };
        self.idempotent_connector_write_outcome(method, url, ConnectorWriteBody::Empty, operation)
            .await
    }

    pub async fn add_reaction_outcome(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        reaction: &str,
    ) -> WriteOutcome {
        self.reaction_activity_outcome(
            reqwest::Method::PUT,
            service_url,
            conversation_id,
            activity_id,
            reaction,
            "Bot Framework add reaction",
        )
        .await
    }

    pub async fn remove_reaction_outcome(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        reaction: &str,
    ) -> WriteOutcome {
        self.reaction_activity_outcome(
            reqwest::Method::DELETE,
            service_url,
            conversation_id,
            activity_id,
            reaction,
            "Bot Framework remove reaction",
        )
        .await
    }

    /// Compatibility wrapper for callers that predate structured outcomes.
    pub async fn update_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        write_outcome_to_result(
            self.update_activity_outcome(service_url, conversation_id, activity_id, text)
                .await,
        )
    }

    /// Compatibility wrapper for callers that predate structured outcomes.
    pub async fn delete_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
    ) -> anyhow::Result<()> {
        write_outcome_to_result(
            self.delete_activity_outcome(service_url, conversation_id, activity_id)
                .await,
        )
    }
}

fn build_http_client(request_timeout: Duration) -> reqwest::Client {
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= TEAMS_MAX_REDIRECTS {
            return attempt.stop();
        }
        let Some(previous) = attempt.previous().last() else {
            return attempt.stop();
        };
        let target = attempt.url();
        let same_origin = previous.scheme() == target.scheme()
            && previous.host_str() == target.host_str()
            && previous.port_or_known_default() == target.port_or_known_default();
        let safe_authority = target.username().is_empty() && target.password().is_none();
        if same_origin && safe_authority {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });

    reqwest::Client::builder()
        .connect_timeout(TEAMS_CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .redirect(redirect_policy)
        .build()
        .unwrap_or_else(|error| panic!("teams: failed to build hardened HTTP client: {error}"))
}

fn build_attachment_http_client(request_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(TEAMS_CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|error| panic!("teams: failed to build attachment HTTP client: {error}"))
}

fn validate_public_cloud_endpoint(
    raw_url: &str,
    label: &str,
    expected_host: &str,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    let url =
        reqwest::Url::parse(raw_url).map_err(|_| anyhow::anyhow!("{label} is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("{label} must not contain userinfo");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("{label} must not contain a query or fragment");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{label} is missing a host"))?;

    if allow_non_public_endpoints {
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("{label} must use HTTP or HTTPS in tests");
        }
        return Ok(url);
    }

    if url.scheme() != "https" {
        anyhow::bail!("{label} must use HTTPS");
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        anyhow::bail!("{label} must not use an IP literal");
    }
    if !host.eq_ignore_ascii_case(expected_host) {
        anyhow::bail!("{label} host is not allowed for Microsoft public cloud");
    }
    if url.port_or_known_default() != Some(443) {
        anyhow::bail!("{label} must use HTTPS port 443");
    }
    Ok(url)
}

fn connector_url(
    service_url: &str,
    conversation_id: &str,
    activity_id: Option<&str>,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    validate_connector_id(conversation_id, "conversation ID")?;
    if let Some(activity_id) = activity_id {
        validate_connector_id(activity_id, "activity ID")?;
    }

    let mut url = validate_public_cloud_endpoint(
        service_url,
        "Teams service URL",
        TEAMS_PUBLIC_SERVICE_HOST,
        allow_non_public_endpoints,
    )?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Teams service URL cannot be used as a base URL"))?;
        segments.pop_if_empty();
        segments
            .push("v3")
            .push("conversations")
            .push(conversation_id)
            .push("activities");
        if let Some(activity_id) = activity_id {
            segments.push(activity_id);
        }
    }
    Ok(url)
}

fn teams_reaction_type(value: &str) -> Option<Cow<'_, str>> {
    let mapped = match value {
        "👀" => "1f440_eyes",
        "🤔" => "think",
        "🔥" => "fire",
        "👨‍💻" => "mantechie",
        "⚡" | "⚡️" => "26a1_highvoltagesign",
        "🆗" => "1f197_squaredok",
        "🥱" => "1f971_yawningface",
        "😨" => "fearful",
        "😱" => "screamingfear",
        "😊" => "smileeyes",
        "😎" => "cool",
        "🫡" => "salute",
        "🤓" => "nerdy",
        "😏" => "smirk",
        "✌" | "✌️" => "victory",
        "💪" => "muscle",
        "🦾" => "1f9be_mechanicalarm",
        "👍" => "like",
        "❤" | "❤️" => "heart",
        "✅" => "2705_whiteheavycheckmark",
        "❌" => "274c_crossmark",
        "⏳" => "holdon",
        value
            if value.len() <= 128
                && !value.is_empty()
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                }) =>
        {
            return Some(Cow::Borrowed(value));
        }
        _ => return None,
    };
    Some(Cow::Borrowed(mapped))
}

fn reaction_url(
    service_url: &str,
    conversation_id: &str,
    activity_id: &str,
    reaction_type: &str,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    validate_connector_id(reaction_type, "reaction type")?;
    let mut url = connector_url(
        service_url,
        conversation_id,
        Some(activity_id),
        allow_non_public_endpoints,
    )?;
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Teams service URL cannot be used as a base URL"))?;
    segments.push("reactions").push(reaction_type);
    drop(segments);
    Ok(url)
}

fn validate_connector_id(id: &str, label: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("Teams {label} must not be empty");
    }
    if matches!(id, "." | "..") {
        anyhow::bail!("Teams {label} must not be a dot segment");
    }
    Ok(())
}

fn safe_request_error(operation: &str, error: &reqwest::Error) -> anyhow::Error {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_redirect() {
        "redirect failed"
    } else {
        "request failed"
    };
    anyhow::anyhow!("{operation} {kind}")
}

fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1000));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    let delay = retry_at
        .duration_since(std::time::SystemTime::now())
        .unwrap_or_default();
    Some(delay.as_millis().min(u128::from(u64::MAX)) as u64)
}

async fn classify_write_failure(
    response: reqwest::Response,
    operation: &str,
    sensitive_values: &[&str],
) -> WriteOutcome {
    let status = response.status();
    let retry_after_ms = (status == StatusCode::TOO_MANY_REQUESTS)
        .then(|| parse_retry_after_ms(response.headers()))
        .flatten();
    let body = read_bounded_error_body(response, sensitive_values).await;
    let message = format!("{operation} failed with HTTP {status}: {body}");
    if status.is_server_error() {
        WriteOutcome::Unknown {
            code: "connector_server_error".into(),
            message,
        }
    } else {
        let code = match status.as_u16() {
            401 | 403 => "authorization_rejected",
            413 => "message_too_large",
            429 => "rate_limited",
            300..=399 => "redirect_rejected",
            _ => "connector_rejected",
        };
        WriteOutcome::Rejected {
            code: code.into(),
            message,
            retry_after_ms,
        }
    }
}

fn write_outcome_to_result(outcome: WriteOutcome) -> anyhow::Result<()> {
    match outcome {
        WriteOutcome::Delivered { .. } => Ok(()),
        WriteOutcome::Rejected { message, .. } | WriteOutcome::Unknown { message, .. } => {
            Err(anyhow::anyhow!(message))
        }
    }
}

async fn require_http_success(
    response: reqwest::Response,
    operation: &str,
    sensitive_values: &[&str],
) -> anyhow::Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let body = read_bounded_error_body(response, sensitive_values).await;
    anyhow::bail!("{operation} failed with HTTP {status}: {body}")
}

async fn read_bounded_error_body(
    mut response: reqwest::Response,
    sensitive_values: &[&str],
) -> String {
    let mut bytes = Vec::with_capacity(TEAMS_ERROR_BODY_LIMIT);
    let mut truncated = false;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = TEAMS_ERROR_BODY_LIMIT.saturating_sub(bytes.len());
                if remaining == 0 {
                    truncated = true;
                    break;
                }
                let take = remaining.min(chunk.len());
                bytes.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    truncated = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }

    let mut redacted = match String::from_utf8(bytes) {
        Ok(text) => redact_sensitive_text(&text, sensitive_values),
        Err(_) => "[non-UTF-8 error body]".into(),
    };
    truncate_utf8(&mut redacted, TEAMS_ERROR_BODY_LIMIT);
    if redacted.is_empty() {
        redacted.push_str("<empty>");
    }
    if truncated {
        redacted.push_str(" [truncated]");
    }
    redacted
}

fn redact_sensitive_text(input: &str, sensitive_values: &[&str]) -> String {
    let mut value = match serde_json::from_str::<serde_json::Value>(input) {
        Ok(mut value) => {
            redact_sensitive_json(&mut value, sensitive_values);
            serde_json::to_string(&value).unwrap_or_else(|_| "[REDACTED]".into())
        }
        Err(_) => input.to_string(),
    };

    value = redact_urls(&value);
    for sensitive in sensitive_values.iter().filter(|value| !value.is_empty()) {
        value = value.replace(sensitive, "[REDACTED]");
    }
    for marker in [
        "bearer ",
        "access_token=",
        "access_token:",
        "access_token\":\"",
        "refresh_token=",
        "refresh_token:",
        "refresh_token\":\"",
        "client_secret=",
        "client_secret:",
        "client_secret\":\"",
        "authorization\":\"",
    ] {
        value = redact_value_after_marker(&value, marker);
    }
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn redact_sensitive_json(value: &mut serde_json::Value, sensitive_values: &[&str]) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase();
                if key.contains("token") || key.contains("secret") || key == "authorization" {
                    *value = serde_json::Value::String("[REDACTED]".into());
                } else {
                    redact_sensitive_json(value, sensitive_values);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_sensitive_json(value, sensitive_values);
            }
        }
        serde_json::Value::String(string) => {
            *string = redact_urls(string);
            for sensitive in sensitive_values.iter().filter(|value| !value.is_empty()) {
                *string = string.replace(sensitive, "[REDACTED]");
            }
        }
        _ => {}
    }
}

fn redact_urls(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let http = lower[cursor..]
            .find("http://")
            .map(|offset| cursor + offset);
        let https = lower[cursor..]
            .find("https://")
            .map(|offset| cursor + offset);
        let Some(start) = [http, https].into_iter().flatten().min() else {
            output.push_str(&input[cursor..]);
            break;
        };
        output.push_str(&input[cursor..start]);
        output.push_str("[REDACTED_URL]");
        let mut end = input.len();
        for (offset, character) in input[start..].char_indices() {
            if offset > 0
                && (character.is_whitespace()
                    || matches!(
                        character,
                        '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}'
                    ))
            {
                end = start + offset;
                break;
            }
        }
        cursor = end;
    }
    output
}

fn redact_value_after_marker(input: &str, marker: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find(marker) {
        let start = cursor + relative_start;
        let value_start = start + marker.len();
        output.push_str(&input[cursor..value_start]);
        output.push_str("[REDACTED]");

        let mut end = input.len();
        for (offset, character) in input[value_start..].char_indices() {
            if character.is_whitespace()
                || matches!(character, '"' | '\'' | '&' | ',' | ';' | '}' | ']')
            {
                end = value_start + offset;
                break;
            }
        }
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

const TEAMS_FILE_DOWNLOAD_INFO_TYPE: &str = "application/vnd.microsoft.teams.file.download.info";
const TEAMS_ATTACHMENT_MAX_REDIRECTS: usize = 4;
const TEAMS_FILE_HOST_SUFFIXES: &[&str] = &[
    "api.asm.skype.com",
    "files.teams.microsoft.com",
    "sharepoint.com",
    "sharepointonline.com",
    "1drv.com",
    "onedrive.com",
    "blob.core.windows.net",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamsFileDownloadInfo {
    download_url: String,
    #[serde(default)]
    file_size: Option<u64>,
}

#[derive(Default)]
struct PreparedTeamsAttachments {
    metadata: Vec<Attachment>,
    sources: HashMap<String, TeamsAttachmentSource>,
}

struct AttachmentFailure {
    category: &'static str,
    detail: &'static str,
    bytes_read: u64,
}

impl AttachmentFailure {
    fn new(category: &'static str, detail: &'static str) -> Self {
        Self {
            category,
            detail,
            bytes_read: 0,
        }
    }

    fn with_bytes_read(mut self, bytes_read: u64) -> Self {
        self.bytes_read = bytes_read;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttachmentMaterializationError {
    code: &'static str,
    message: &'static str,
}

impl AttachmentMaterializationError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for AttachmentMaterializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for AttachmentMaterializationError {}

fn sanitize_attachment_filename(value: Option<&str>, fallback: &str) -> String {
    let mut sanitized: String = value
        .unwrap_or_default()
        .chars()
        .filter_map(|character| match character {
            '/' | '\\' => Some('_'),
            character if character.is_control() => None,
            character => Some(character),
        })
        .take(TEAMS_FILENAME_LIMIT)
        .collect();
    sanitized = sanitized.trim().to_owned();
    if sanitized.is_empty() {
        fallback.to_owned()
    } else {
        sanitized
    }
}

fn sanitized_declared_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '+' | '-' | '.')
        })
        .take(128)
        .collect()
}

fn image_mime_for_filename(filename: &str) -> Option<&'static str> {
    let extension = filename.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn rejected_attachment(
    attachment_type: &str,
    filename: String,
    mime_type: String,
    size: u64,
    category: &'static str,
    detail: &'static str,
) -> Attachment {
    Attachment {
        attachment_type: attachment_type.into(),
        filename,
        mime_type,
        reference: None,
        data: String::new(),
        size,
        path: None,
        status: Some(format!("{category}: {detail}")),
    }
}

fn parse_file_download_info(content: Option<&serde_json::Value>) -> Option<TeamsFileDownloadInfo> {
    match content? {
        serde_json::Value::String(value) => serde_json::from_str(value).ok(),
        value => serde_json::from_value(value.clone()).ok(),
    }
}

fn attachment_url_base(
    raw_url: &str,
    label: &str,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    let url =
        reqwest::Url::parse(raw_url).map_err(|_| anyhow::anyhow!("{label} is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("{label} must not contain userinfo");
    }
    if url.fragment().is_some() {
        anyhow::bail!("{label} must not contain a fragment");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{label} is missing a host"))?;
    if allow_non_public_endpoints {
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("{label} must use HTTP or HTTPS in tests");
        }
        return Ok(url);
    }
    if url.scheme() != "https" {
        anyhow::bail!("{label} must use HTTPS");
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        anyhow::bail!("{label} must not use an IP literal");
    }
    if url.port_or_known_default() != Some(443) {
        anyhow::bail!("{label} must use HTTPS port 443");
    }
    Ok(url)
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

fn validate_inline_attachment_url(
    raw_url: &str,
    service_origin: &reqwest::Url,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    let url = attachment_url_base(
        raw_url,
        "Teams inline attachment URL",
        allow_non_public_endpoints,
    )?;
    if !same_origin(&url, service_origin) {
        anyhow::bail!("Teams inline attachment URL must match the Connector origin");
    }
    Ok(url)
}

fn is_allowed_file_host(host: &str) -> bool {
    TEAMS_FILE_HOST_SUFFIXES.iter().any(|suffix| {
        host.eq_ignore_ascii_case(suffix)
            || host.to_ascii_lowercase().ends_with(&format!(".{suffix}"))
    })
}

fn validate_file_attachment_url(
    raw_url: &str,
    allow_non_public_endpoints: bool,
) -> anyhow::Result<reqwest::Url> {
    let url = attachment_url_base(
        raw_url,
        "Teams file attachment URL",
        allow_non_public_endpoints,
    )?;
    if allow_non_public_endpoints {
        return Ok(url);
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Teams file attachment URL is missing a host"))?;
    if !is_allowed_file_host(host) {
        anyhow::bail!("Teams file attachment host is not in the public-cloud profile");
    }
    Ok(url)
}

fn prepare_attachment_metadata(
    teams: &TeamsAdapter,
    activity: &Activity,
    service_origin: &reqwest::Url,
    conversation_type: &str,
) -> PreparedTeamsAttachments {
    let mut prepared = PreparedTeamsAttachments::default();
    let explicit_personal_scope = activity
        .conversation
        .as_ref()
        .and_then(|conversation| conversation.conversation_type.as_deref())
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|value| canonical_conversation_type(value) == "personal");
    for attachment in activity
        .attachments
        .iter()
        .take(TEAMS_ATTACHMENT_METADATA_LIMIT)
    {
        let declared_mime = sanitized_declared_mime(&attachment.content_type);
        let filename = sanitize_attachment_filename(attachment.name.as_deref(), "attachment");
        if declared_mime.starts_with("image/") {
            let Some(content_url) = attachment
                .content_url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                prepared.metadata.push(rejected_attachment(
                    "image",
                    filename,
                    declared_mime,
                    0,
                    "invalid content",
                    "inline image has no content URL",
                ));
                continue;
            };
            let url = match validate_inline_attachment_url(
                content_url,
                service_origin,
                teams.allow_non_public_endpoints,
            ) {
                Ok(url) => url,
                Err(_) => {
                    prepared.metadata.push(rejected_attachment(
                        "image",
                        filename,
                        declared_mime,
                        0,
                        "security rejected",
                        "inline image URL is outside the Connector origin",
                    ));
                    continue;
                }
            };
            let reference = format!("att_{}", uuid::Uuid::new_v4());
            prepared.sources.insert(
                reference.clone(),
                TeamsAttachmentSource {
                    kind: TeamsAttachmentSourceKind::InlineImage,
                    url,
                    service_origin: service_origin.clone(),
                    attachment_type: "image".into(),
                    filename: filename.clone(),
                    mime_type: declared_mime.clone(),
                    max_bytes: TEAMS_IMAGE_DOWNLOAD_LIMIT,
                },
            );
            prepared.metadata.push(Attachment {
                attachment_type: "image".into(),
                filename,
                mime_type: declared_mime,
                reference: Some(reference),
                data: String::new(),
                size: 0,
                path: None,
                status: None,
            });
            continue;
        }

        if declared_mime == TEAMS_FILE_DOWNLOAD_INFO_TYPE {
            let Some(info) = parse_file_download_info(attachment.content.as_ref()) else {
                prepared.metadata.push(rejected_attachment(
                    "file",
                    filename,
                    declared_mime,
                    0,
                    "invalid content",
                    "file download metadata is malformed",
                ));
                continue;
            };
            let declared_size = info.file_size.unwrap_or(0);
            if conversation_type != "personal" || !explicit_personal_scope {
                prepared.metadata.push(rejected_attachment(
                    "file",
                    filename,
                    declared_mime,
                    declared_size,
                    "unsupported format",
                    "Teams file download is Personal-only",
                ));
                continue;
            }
            let (kind, attachment_type, normalized_mime, max_bytes) =
                if let Some(image_mime) = image_mime_for_filename(&filename) {
                    (
                        TeamsAttachmentSourceKind::PersonalFileImage,
                        "image",
                        image_mime,
                        TEAMS_IMAGE_DOWNLOAD_LIMIT,
                    )
                } else if crate::media::is_text_extension(&filename) {
                    (
                        TeamsAttachmentSourceKind::PersonalTextFile,
                        "text_file",
                        "text/plain; charset=utf-8",
                        TEAMS_TEXT_DOWNLOAD_LIMIT,
                    )
                } else {
                    prepared.metadata.push(rejected_attachment(
                        "file",
                        filename,
                        declared_mime,
                        declared_size,
                        "unsupported format",
                        "file extension is not supported",
                    ));
                    continue;
                };
            if declared_size > max_bytes {
                prepared.metadata.push(rejected_attachment(
                    attachment_type,
                    filename,
                    normalized_mime.into(),
                    declared_size,
                    "size exceeded",
                    "declared file size exceeds the limit",
                ));
                continue;
            }
            let url = match validate_file_attachment_url(
                &info.download_url,
                teams.allow_non_public_endpoints,
            ) {
                Ok(url) => url,
                Err(_) => {
                    prepared.metadata.push(rejected_attachment(
                        attachment_type,
                        filename,
                        normalized_mime.into(),
                        declared_size,
                        "security rejected",
                        "file URL is outside the public-cloud profile",
                    ));
                    continue;
                }
            };
            let reference = format!("att_{}", uuid::Uuid::new_v4());
            prepared.sources.insert(
                reference.clone(),
                TeamsAttachmentSource {
                    kind,
                    url,
                    service_origin: service_origin.clone(),
                    attachment_type: attachment_type.into(),
                    filename: filename.clone(),
                    mime_type: normalized_mime.into(),
                    max_bytes,
                },
            );
            prepared.metadata.push(Attachment {
                attachment_type: attachment_type.into(),
                filename,
                mime_type: normalized_mime.into(),
                reference: Some(reference),
                data: String::new(),
                size: declared_size,
                path: None,
                status: None,
            });
            continue;
        }

        if declared_mime.starts_with("application/vnd.microsoft.card.") {
            continue;
        }
        if attachment.content_url.is_some() || attachment.name.is_some() {
            prepared.metadata.push(rejected_attachment(
                "file",
                filename,
                declared_mime,
                0,
                "unsupported format",
                "attachment type is not supported",
            ));
        }
    }
    prepared
}

fn materialization_protocol_error(error: AttachmentLookupError) -> AttachmentMaterializationError {
    match error {
        AttachmentLookupError::RouteNotFound => AttachmentMaterializationError {
            code: "attachment_route_not_found",
            message: "attachment route is unavailable",
        },
        AttachmentLookupError::ConversationMismatch => AttachmentMaterializationError {
            code: "attachment_scope_mismatch",
            message: "attachment conversation does not match its route",
        },
        AttachmentLookupError::ReferenceNotFound => AttachmentMaterializationError {
            code: "attachment_reference_not_found",
            message: "attachment reference is unavailable",
        },
        AttachmentLookupError::AggregateLimitExceeded => AttachmentMaterializationError {
            code: "attachment_budget_exceeded",
            message: "attachment event budget is exhausted",
        },
    }
}

impl TeamsAdapter {
    async fn download_attachment_bytes(
        &self,
        source: &TeamsAttachmentSource,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AttachmentFailure> {
        let bearer = if source.kind == TeamsAttachmentSourceKind::InlineImage {
            Some(self.get_token().await.map_err(|_| {
                AttachmentFailure::new("download failed", "Bot token is unavailable")
            })?)
        } else {
            None
        };
        let mut url = source.url.clone();
        let mut redirects = 0usize;
        loop {
            let mut request = self.attachment_client.get(url.clone());
            if let Some(token) = bearer.as_deref() {
                request = request.bearer_auth(token);
            }
            let mut response = request.send().await.map_err(|_| {
                AttachmentFailure::new("download failed", "attachment request failed")
            })?;
            if response.status().is_redirection() {
                if redirects >= TEAMS_ATTACHMENT_MAX_REDIRECTS {
                    return Err(AttachmentFailure::new(
                        "security rejected",
                        "attachment redirect limit exceeded",
                    ));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        AttachmentFailure::new(
                            "download failed",
                            "attachment redirect has no valid location",
                        )
                    })?;
                let candidate = url.join(location).map_err(|_| {
                    AttachmentFailure::new("security rejected", "attachment redirect is invalid")
                })?;
                url = match source.kind {
                    TeamsAttachmentSourceKind::InlineImage => validate_inline_attachment_url(
                        candidate.as_str(),
                        &source.service_origin,
                        self.allow_non_public_endpoints,
                    ),
                    TeamsAttachmentSourceKind::PersonalFileImage
                    | TeamsAttachmentSourceKind::PersonalTextFile => validate_file_attachment_url(
                        candidate.as_str(),
                        self.allow_non_public_endpoints,
                    ),
                }
                .map_err(|_| {
                    AttachmentFailure::new(
                        "security rejected",
                        "attachment redirect is outside the allowed origin profile",
                    )
                })?;
                redirects += 1;
                continue;
            }
            if !response.status().is_success() {
                return Err(AttachmentFailure::new(
                    "download failed",
                    "Microsoft attachment response was not successful",
                ));
            }
            if response
                .content_length()
                .is_some_and(|size| size > max_bytes)
            {
                return Err(AttachmentFailure::new(
                    "size exceeded",
                    "attachment Content-Length exceeds the limit",
                ));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                AttachmentFailure::new("download failed", "attachment body read failed")
                    .with_bytes_read(bytes.len() as u64)
            })? {
                let next_len = bytes.len().saturating_add(chunk.len());
                if next_len as u64 > max_bytes {
                    return Err(AttachmentFailure::new(
                        "size exceeded",
                        "attachment body exceeds the limit",
                    )
                    .with_bytes_read(max_bytes));
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(bytes);
        }
    }

    pub async fn materialize_attachment(
        &self,
        event_id: &str,
        conversation_id: &str,
        reference: &str,
    ) -> Result<Attachment, AttachmentMaterializationError> {
        if !self.inbound_attachments_enabled() {
            return Err(AttachmentMaterializationError {
                code: "attachment_materialization_disabled",
                message: "attachment materialization is disabled",
            });
        }
        let claim = self
            .ingress
            .lock()
            .await
            .claim_attachment(event_id, conversation_id, reference, Instant::now())
            .map_err(materialization_protocol_error)?;
        let download = self
            .download_attachment_bytes(&claim.source, claim.reserved_bytes)
            .await;
        let raw_bytes = download
            .as_ref()
            .map(|bytes| bytes.len() as u64)
            .unwrap_or_else(|failure| failure.bytes_read);
        self.ingress
            .lock()
            .await
            .finish_attachment(event_id, claim.reserved_bytes, raw_bytes);

        let bytes = match download {
            Ok(bytes) => bytes,
            Err(failure) => {
                return Ok(rejected_attachment(
                    &claim.source.attachment_type,
                    claim.source.filename,
                    claim.source.mime_type,
                    raw_bytes,
                    failure.category,
                    failure.detail,
                ));
            }
        };
        let normalized = match claim.source.kind {
            TeamsAttachmentSourceKind::InlineImage
            | TeamsAttachmentSourceKind::PersonalFileImage => {
                match tokio::task::spawn_blocking(move || {
                    crate::media::resize_and_compress(&bytes)
                })
                .await
                {
                    Ok(result) => result.map_err(|_| {
                        AttachmentFailure::new(
                            "processing failed",
                            "image decoding or normalization failed",
                        )
                    }),
                    Err(_) => Err(AttachmentFailure::new(
                        "processing failed",
                        "image normalization task failed",
                    )),
                }
            }
            TeamsAttachmentSourceKind::PersonalTextFile => {
                if std::str::from_utf8(&bytes).is_err() {
                    Err(AttachmentFailure::new(
                        "invalid content",
                        "text attachment is not valid UTF-8",
                    ))
                } else {
                    Ok((bytes, "text/plain; charset=utf-8".into()))
                }
            }
        };
        let (normalized_bytes, mime_type) = match normalized {
            Ok(normalized) => normalized,
            Err(failure) => {
                return Ok(rejected_attachment(
                    &claim.source.attachment_type,
                    claim.source.filename,
                    claim.source.mime_type,
                    raw_bytes,
                    failure.category,
                    failure.detail,
                ));
            }
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(&normalized_bytes);
        if encoded.len().saturating_add(4096) > TEAMS_MATERIALIZED_FRAME_LIMIT {
            return Ok(rejected_attachment(
                &claim.source.attachment_type,
                claim.source.filename,
                mime_type,
                raw_bytes,
                "size exceeded",
                "normalized attachment exceeds the internal frame limit",
            ));
        }
        Ok(Attachment {
            attachment_type: claim.source.attachment_type,
            filename: claim.source.filename,
            mime_type,
            reference: None,
            data: encoded,
            size: normalized_bytes.len() as u64,
            path: None,
            status: None,
        })
    }
}

// --- Webhook handler ---

/// Max webhook body size: 256 KB. Real Teams activities are a few KB; the
/// activity is parsed *before* JWT auth (Bot Framework requires serviceUrl /
/// channelId from the body to validate the token), so this caps the
/// unauthenticated parse attack surface. Mirrors the feishu adapter's limit.
const WEBHOOK_BODY_LIMIT: usize = 256 * 1024;

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let teams = match &state.teams {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND,
    };

    // Defense-in-depth: bound the pre-auth body size (axum's default limit is 2 MB).
    if body.len() > WEBHOOK_BODY_LIMIT {
        warn!(size = body.len(), "teams webhook body too large");
        return StatusCode::PAYLOAD_TOO_LARGE;
    }

    // Extract auth header early (before parsing activity)
    let auth_header = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(h) => h.to_string(),
        None => {
            warn!("teams webhook: missing authorization header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    // Parse activity first (needed for JWT serviceUrl + endorsements validation).
    //
    // SECURITY NOTE (OX untrusted-deserialization finding — false positive):
    // `Activity` is a strict, derive-only DTO (String / Option<_> / nested
    // structs) with no custom Deserialize, no side-effectful Drop, and no enum
    // variant dispatch. serde_json's data model cannot instantiate arbitrary
    // types (unlike bincode/serde_yaml/rmp-serde), so object-injection / RCE
    // does not apply. The recommended "strict DTO + validate after" pattern is
    // already in place: JWT, activity-type, and tenant-allowlist checks below.
    // DoS is bounded by serde_json's recursion limit (128) and the body cap above.
    let activity: Activity = match serde_json::from_str(&body) {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "teams: invalid activity JSON");
            return StatusCode::BAD_REQUEST;
        }
    };

    if activity.activity_type == "message" {
        if let Some(field) = activity.missing_required_message_field() {
            warn!(field, "teams: message missing required field");
            return StatusCode::BAD_REQUEST;
        }
    }

    // JWT validation (with activity context for serviceUrl + channelId checks)
    if let Err(e) = teams.validate_jwt(&auth_header, &activity).await {
        warn!(error = %e, "teams JWT validation failed");
        return StatusCode::UNAUTHORIZED;
    }

    // Only handle message activities
    if activity.activity_type != "message" {
        debug!(activity_type = %activity.activity_type, "teams: ignoring non-message activity");
        return StatusCode::OK;
    }

    // Tenant check
    if !teams.check_tenant(&activity) {
        let tid = activity.resolved_tenant_id().unwrap_or("unknown");
        warn!(tenant = tid, "teams: tenant not in allowlist");
        return StatusCode::FORBIDDEN;
    }

    accept_message_activity(state, activity).await
}

enum LocalPublishOutcome {
    Accepted { receiver_count: usize },
    AcceptedDuplicate,
    PublishingDuplicate(tokio::sync::watch::Receiver<PublishState>),
    AtCapacity,
    NoConsumer,
    StateCommitFailed,
}

/// Publish one already-authenticated and tenant-authorized Teams message.
///
/// Keeping this post-auth path separate makes the local enqueue, route, and
/// dedupe contract testable without weakening JWT validation in `webhook`.
async fn accept_message_activity(state: Arc<crate::AppState>, activity: Activity) -> StatusCode {
    let Some(teams) = state.teams.as_ref() else {
        return StatusCode::NOT_FOUND;
    };
    if let Some(field) = activity.missing_required_message_field() {
        warn!(field, "teams: message missing required field");
        return StatusCode::BAD_REQUEST;
    }

    let text = activity.text.as_deref().unwrap_or_default().trim();
    if text.is_empty() && (!teams.inbound_attachments_enabled() || activity.attachments.is_empty())
    {
        return StatusCode::OK;
    }
    let Some(tenant_id) = activity
        .resolved_tenant_id()
        .filter(|value| !value.trim().is_empty())
    else {
        warn!("teams: message missing required tenant id");
        return StatusCode::BAD_REQUEST;
    };
    let Some(conversation_id) = activity
        .conversation
        .as_ref()
        .and_then(|conversation| conversation.id.as_deref())
        .filter(|value| !value.trim().is_empty())
    else {
        warn!("teams: message missing required conversation id");
        return StatusCode::BAD_REQUEST;
    };
    let Some(activity_id) = activity
        .id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        warn!("teams: message missing required activity id");
        return StatusCode::BAD_REQUEST;
    };
    let Some(sender_id) = activity
        .from
        .as_ref()
        .and_then(|sender| sender.id.as_deref())
        .filter(|value| !value.trim().is_empty())
    else {
        warn!("teams: message missing required sender id");
        return StatusCode::BAD_REQUEST;
    };
    let Some(service_url) = activity
        .service_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        warn!("teams: message missing required service URL");
        return StatusCode::BAD_REQUEST;
    };

    // JWT validation binds this value to Microsoft; the public-cloud policy
    // additionally prevents credential-bearing SSRF before local persistence.
    let validated_service_url = match teams.validate_service_url(service_url) {
        Ok(url) => url,
        Err(error) => {
            warn!(reason = %error, "teams: activity has unsafe service_url");
            return StatusCode::BAD_REQUEST;
        }
    };
    let conversation_type = canonical_conversation_type(
        activity
            .conversation
            .as_ref()
            .and_then(|conversation| conversation.conversation_type.as_deref())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("personal"),
    );
    let prepared_attachments = if teams.inbound_attachments_enabled() {
        prepare_attachment_metadata(teams, &activity, &validated_service_url, &conversation_type)
    } else {
        PreparedTeamsAttachments::default()
    };
    if text.is_empty() && prepared_attachments.metadata.is_empty() {
        return StatusCode::OK;
    }
    let sender_name = activity
        .from
        .as_ref()
        .and_then(|sender| sender.name.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Unknown");
    let scope = activity.gateway_scope(tenant_id, conversation_id, &conversation_type);
    let route_team_id = scope.team_id.clone();
    let route_channel_id = scope.channel_id.clone();
    let recipient = activity.recipient_info();
    let (mentions, mention_entities) = activity.mention_info();

    let mut event = GatewayEvent::new(
        "teams",
        ChannelInfo {
            id: conversation_id.to_owned(),
            channel_type: conversation_type.clone(),
            thread_id: None,
        },
        SenderInfo {
            id: sender_id.to_owned(),
            name: sender_name.to_owned(),
            display_name: sender_name.to_owned(),
            is_bot: false,
        },
        text,
        activity_id,
        mentions,
    );
    event.scope = Some(scope);
    event.recipient = recipient;
    event.mention_entities = mention_entities;
    event.content.attachments = prepared_attachments.metadata;
    let event_id = event.event_id.clone();
    let route_key = TeamsRouteKey::new(
        teams.config.app_id.clone(),
        tenant_id,
        conversation_id,
        activity_id,
    );
    let now = Instant::now();
    let route = TeamsIngressRoute {
        key: route_key.clone(),
        event_id: event_id.clone(),
        tenant_id: tenant_id.to_owned(),
        conversation_id: conversation_id.to_owned(),
        conversation_type: conversation_type.clone(),
        inbound_activity_id: activity_id.to_owned(),
        reply_chain_root_id: activity.reply_to_id.clone(),
        service_url: validated_service_url.clone(),
        team_id: route_team_id,
        channel_id: route_channel_id,
        attachment_sources: prepared_attachments.sources,
        attachment_materialized_bytes: 0,
        created_at: now,
    };
    let json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(serialization_error) => {
            error!(error = %serialization_error, "teams: failed to serialize gateway event");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // Reserve, commit the route, and enqueue while holding one state lock. No
    // await point exists after Publishing begins, so cancellation cannot leave
    // an owner stranded between local enqueue and Accepted/Failed resolution.
    let publish_outcome = {
        let mut ingress = teams.ingress.lock().await;
        match ingress.reserve(route_key.clone(), event_id.clone(), now) {
            PublishReservation::AcceptedDuplicate => LocalPublishOutcome::AcceptedDuplicate,
            PublishReservation::PublishingDuplicate(completion) => {
                LocalPublishOutcome::PublishingDuplicate(completion)
            }
            PublishReservation::AtCapacity => LocalPublishOutcome::AtCapacity,
            PublishReservation::Owner => {
                if !ingress.accept(&route_key, &event_id, route, Instant::now()) {
                    ingress.fail(&route_key, &event_id);
                    LocalPublishOutcome::StateCommitFailed
                } else {
                    match state.event_tx.send(json) {
                        Ok(receiver_count) => LocalPublishOutcome::Accepted { receiver_count },
                        Err(_) => {
                            ingress.fail(&route_key, &event_id);
                            LocalPublishOutcome::NoConsumer
                        }
                    }
                }
            }
        }
    };

    match publish_outcome {
        LocalPublishOutcome::Accepted { receiver_count } => {
            info!(
                conversation = conversation_id,
                sender = sender_name,
                tenant = tenant_id,
                service_host = validated_service_url.host_str().unwrap_or("unknown"),
                receiver_count,
                "teams → gateway"
            );
            StatusCode::OK
        }
        LocalPublishOutcome::AcceptedDuplicate => {
            debug!("teams: accepted duplicate activity suppressed");
            StatusCode::OK
        }
        LocalPublishOutcome::PublishingDuplicate(completion) => {
            match wait_for_publish(completion).await {
                PublishState::Accepted => StatusCode::OK,
                PublishState::Publishing | PublishState::Failed => StatusCode::SERVICE_UNAVAILABLE,
            }
        }
        LocalPublishOutcome::AtCapacity => {
            warn!("teams: ingress dedupe cache is saturated by active publications");
            StatusCode::SERVICE_UNAVAILABLE
        }
        LocalPublishOutcome::NoConsumer => {
            warn!("teams: no event consumer accepted the activity; returning retryable failure");
            StatusCode::SERVICE_UNAVAILABLE
        }
        LocalPublishOutcome::StateCommitFailed => {
            error!("teams: failed to commit ingress state before local enqueue");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// --- Reply handler ---

fn rejected_outcome(code: &str, message: impl Into<String>) -> WriteOutcome {
    WriteOutcome::Rejected {
        code: code.into(),
        message: message.into(),
        retry_after_ms: None,
    }
}

async fn resolve_send_route(
    teams: &TeamsAdapter,
    reply: &GatewayReply,
) -> Result<(TeamsIngressRoute, Option<String>), WriteOutcome> {
    let result = teams.ingress.lock().await.route_for_reply(
        &reply.reply_to,
        &reply.channel.id,
        reply.quote_message_id.as_deref(),
        Instant::now(),
    );
    match result {
        Ok(route) => Ok(route),
        Err(RouteLookupError::NotFound) => Err(rejected_outcome(
            "route_not_found",
            "Teams ingress route is missing or expired",
        )),
        Err(RouteLookupError::ConversationMismatch) => Err(rejected_outcome(
            "route_mismatch",
            "Teams reply conversation does not match its ingress route",
        )),
    }
}

async fn handle_send_reply(reply: &GatewayReply, teams: &TeamsAdapter) -> WriteOutcome {
    let (route, _) = match resolve_send_route(teams, reply).await {
        Ok(route) => route,
        Err(outcome) => return outcome,
    };
    let _write_guard = teams.lock_conversation(&route).await;
    let (route, quote_activity_id) = match resolve_send_route(teams, reply).await {
        Ok(route) => route,
        Err(outcome) => return outcome,
    };

    if reply.quote_message_id.is_some() && quote_activity_id.is_none() {
        warn!(
            conversation = %route.conversation_id,
            "teams: quote target is not known in the ingress route scope; sending without quote"
        );
    }

    info!(conversation = %route.conversation_id, "gateway → teams");
    let outcome = teams
        .send_activity_outcome(
            route.service_url.as_str(),
            &route.conversation_id,
            &reply.content.text,
            quote_activity_id.as_deref(),
        )
        .await;
    if let WriteOutcome::Delivered {
        message_id: Some(activity_id),
    } = &outcome
    {
        teams
            .ingress
            .lock()
            .await
            .record_owned(&route, activity_id, Instant::now());
        debug!(activity_id, "teams activity sent and ownership recorded");
    }
    outcome
}

fn command_target(reply: &GatewayReply) -> Result<(&str, Option<&str>), WriteOutcome> {
    match reply.target_message_id.as_deref() {
        Some(target) if target.trim().is_empty() => Err(rejected_outcome(
            "invalid_target",
            "Teams command target must not be empty",
        )),
        Some(target) => Ok((target, Some(reply.reply_to.as_str()))),
        None if reply.reply_to.trim().is_empty() => Err(rejected_outcome(
            "invalid_target",
            "Teams command is missing a target message ID",
        )),
        None => Ok((reply.reply_to.as_str(), None)),
    }
}

async fn resolve_owned_route(
    teams: &TeamsAdapter,
    reply: &GatewayReply,
    target_activity_id: &str,
    origin_event_id: Option<&str>,
) -> Result<TeamsIngressRoute, WriteOutcome> {
    let result = teams.ingress.lock().await.owned_route_for_target(
        &teams.config.app_id,
        origin_event_id,
        &reply.channel.id,
        target_activity_id,
        Instant::now(),
    );
    match result {
        Ok(route) => Ok(route),
        Err(OwnershipLookupError::NotOwned) => Err(rejected_outcome(
            "message_not_owned",
            "Teams target is not a bot-owned activity in this process",
        )),
        Err(OwnershipLookupError::OriginRouteNotFound) => Err(rejected_outcome(
            "target_origin_not_found",
            "Teams command origin route is missing or expired",
        )),
        Err(OwnershipLookupError::ConversationMismatch) => Err(rejected_outcome(
            "target_scope_mismatch",
            "Teams command conversation does not match its origin route",
        )),
        Err(OwnershipLookupError::AmbiguousScope) => Err(rejected_outcome(
            "target_scope_ambiguous",
            "Teams legacy command target is ambiguous across tenant scope",
        )),
    }
}

async fn handle_owned_mutation(
    reply: &GatewayReply,
    teams: &TeamsAdapter,
    command: &str,
) -> WriteOutcome {
    let (target_activity_id, origin_event_id) = match command_target(reply) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let route = match resolve_owned_route(teams, reply, target_activity_id, origin_event_id).await {
        Ok(route) => route,
        Err(outcome) => return outcome,
    };
    let _write_guard = teams.lock_conversation(&route).await;
    let route = match resolve_owned_route(teams, reply, target_activity_id, origin_event_id).await {
        Ok(route) => route,
        Err(outcome) => return outcome,
    };

    info!(conversation = %route.conversation_id, command, "gateway → teams mutation");
    let outcome = match command {
        "edit_message" => {
            teams
                .update_activity_outcome(
                    route.service_url.as_str(),
                    &route.conversation_id,
                    target_activity_id,
                    &reply.content.text,
                )
                .await
        }
        "delete_message" => {
            teams
                .delete_activity_outcome(
                    route.service_url.as_str(),
                    &route.conversation_id,
                    target_activity_id,
                )
                .await
        }
        _ => unreachable!("owned mutation dispatch is command-checked"),
    };
    if command == "delete_message" && matches!(outcome, WriteOutcome::Delivered { .. }) {
        teams
            .ingress
            .lock()
            .await
            .remove_owned(&route, target_activity_id);
    }
    outcome
}

async fn resolve_reaction_route(
    teams: &TeamsAdapter,
    reply: &GatewayReply,
    target_activity_id: &str,
    origin_event_id: Option<&str>,
) -> Result<TeamsIngressRoute, WriteOutcome> {
    let result = teams.ingress.lock().await.route_for_reaction_target(
        &teams.config.app_id,
        origin_event_id,
        &reply.channel.id,
        target_activity_id,
        Instant::now(),
    );
    match result {
        Ok(route) => Ok(route),
        Err(ReactionLookupError::TargetNotKnown) => Err(rejected_outcome(
            "reaction_target_not_known",
            "Teams reaction target is not authenticated in this process",
        )),
        Err(ReactionLookupError::OriginRouteNotFound) => Err(rejected_outcome(
            "target_origin_not_found",
            "Teams reaction origin route is missing or expired",
        )),
        Err(ReactionLookupError::ConversationMismatch) => Err(rejected_outcome(
            "target_scope_mismatch",
            "Teams reaction conversation does not match its origin route",
        )),
        Err(ReactionLookupError::AmbiguousScope) => Err(rejected_outcome(
            "target_scope_ambiguous",
            "Teams legacy reaction target is ambiguous across tenant scope",
        )),
    }
}

async fn handle_reaction(
    reply: &GatewayReply,
    teams: &TeamsAdapter,
    command: &str,
) -> WriteOutcome {
    if !teams.reactions_enabled() {
        debug!(
            command,
            "teams: reaction preview is disabled; ignoring command"
        );
        return WriteOutcome::Delivered { message_id: None };
    }

    let (target_activity_id, origin_event_id) = match command_target(reply) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let route =
        match resolve_reaction_route(teams, reply, target_activity_id, origin_event_id).await {
            Ok(route) => route,
            Err(outcome) => return outcome,
        };
    let _write_guard = teams.lock_conversation(&route).await;
    let route =
        match resolve_reaction_route(teams, reply, target_activity_id, origin_event_id).await {
            Ok(route) => route,
            Err(outcome) => return outcome,
        };

    info!(conversation = %route.conversation_id, command, "gateway → teams reaction");
    match command {
        "add_reaction" => {
            teams
                .add_reaction_outcome(
                    route.service_url.as_str(),
                    &route.conversation_id,
                    target_activity_id,
                    &reply.content.text,
                )
                .await
        }
        "remove_reaction" => {
            teams
                .remove_reaction_outcome(
                    route.service_url.as_str(),
                    &route.conversation_id,
                    target_activity_id,
                    &reply.content.text,
                )
                .await
        }
        _ => unreachable!("reaction dispatch is command-checked"),
    }
}

pub async fn handle_reply(reply: &GatewayReply, teams: &TeamsAdapter) -> WriteOutcome {
    match reply.command.as_deref() {
        None => handle_send_reply(reply, teams).await,
        Some(command @ ("edit_message" | "delete_message")) => {
            handle_owned_mutation(reply, teams, command).await
        }
        Some(command @ ("add_reaction" | "remove_reaction")) => {
            handle_reaction(reply, teams, command).await
        }
        Some(command) => rejected_outcome(
            "unsupported_command",
            format!("unsupported Teams command: {command}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    // --- Bot Connector URL and error hardening ---

    #[test]
    fn connector_url_encodes_segments_and_preserves_service_path() -> anyhow::Result<()> {
        let url = connector_url(
            "https://smba.trafficmanager.net/teams/",
            "a/b?c",
            Some("message id/%"),
            false,
        )?;
        assert_eq!(
            url.as_str(),
            "https://smba.trafficmanager.net/teams/v3/conversations/a%2Fb%3Fc/activities/message%20id%2F%25"
        );
        Ok(())
    }

    #[test]
    fn reaction_url_and_default_status_emojis_use_teams_ids() -> anyhow::Result<()> {
        let url = reaction_url(
            "https://smba.trafficmanager.net/teams/",
            "a/b?c",
            "message id/%",
            "1f440_eyes",
            false,
        )?;
        assert_eq!(
            url.as_str(),
            "https://smba.trafficmanager.net/teams/v3/conversations/a%2Fb%3Fc/activities/message%20id%2F%25/reactions/1f440_eyes"
        );
        for (emoji, expected) in [
            ("👀", "1f440_eyes"),
            ("🤔", "think"),
            ("🔥", "fire"),
            ("👨‍💻", "mantechie"),
            ("⚡", "26a1_highvoltagesign"),
            ("🆗", "1f197_squaredok"),
            ("🥱", "1f971_yawningface"),
            ("😨", "fearful"),
            ("😱", "screamingfear"),
            ("🫡", "salute"),
            ("✅", "2705_whiteheavycheckmark"),
        ] {
            assert_eq!(teams_reaction_type(emoji).as_deref(), Some(expected));
        }
        assert_eq!(
            teams_reaction_type("1f44b_wavinghand-tone4").as_deref(),
            Some("1f44b_wavinghand-tone4")
        );
        assert!(teams_reaction_type("not/a/reaction").is_none());
        Ok(())
    }

    #[test]
    fn service_url_policy_accepts_only_public_teams_connector() {
        assert!(validate_public_cloud_endpoint(
            "https://smba.trafficmanager.net/teams/",
            "Teams service URL",
            TEAMS_PUBLIC_SERVICE_HOST,
            false,
        )
        .is_ok());

        for rejected in [
            "http://smba.trafficmanager.net/teams/",
            "https://user@smba.trafficmanager.net/teams/",
            "https://127.0.0.1/teams/",
            "https://[::1]/teams/",
            "https://localhost/teams/",
            "https://example.com/teams/",
            "https://smba.trafficmanager.net.example.com/teams/",
            "https://smba.trafficmanager.net:8443/teams/",
            "https://smba.trafficmanager.net/teams/?target=other",
            "https://smba.trafficmanager.net/teams/#fragment",
        ] {
            assert!(
                validate_public_cloud_endpoint(
                    rejected,
                    "Teams service URL",
                    TEAMS_PUBLIC_SERVICE_HOST,
                    false,
                )
                .is_err(),
                "unsafe service URL should be rejected"
            );
        }
    }

    #[test]
    fn connector_url_rejects_empty_and_dot_segment_ids() {
        for conversation_id in ["", ".", ".."] {
            assert!(connector_url(
                "https://smba.trafficmanager.net/teams/",
                conversation_id,
                None,
                false,
            )
            .is_err());
        }
        assert!(connector_url(
            "https://smba.trafficmanager.net/teams/",
            "conversation",
            Some(".."),
            false,
        )
        .is_err());
    }

    #[test]
    fn error_text_redacts_tokens_secrets_and_urls() {
        let json = redact_sensitive_text(
            r#"{"access_token":"top-secret","nested":{"client_secret":"also-secret"},"next":"https://sensitive.example/path"}"#,
            &[],
        );
        assert!(!json.contains("top-secret"));
        assert!(!json.contains("also-secret"));
        assert!(!json.contains("sensitive.example"));
        assert!(json.contains("[REDACTED]"));
        assert!(json.contains("[REDACTED_URL]"));

        let truncated_json = redact_sensitive_text(r#"{"access_token":"truncated-secret"#, &[]);
        assert!(!truncated_json.contains("truncated-secret"));

        let plain = redact_sensitive_text(
            "authorization failed: Bearer bearer-secret access_token=query-secret exact-secret https://private.example/path",
            &["exact-secret"],
        );
        assert!(!plain.contains("bearer-secret"));
        assert!(!plain.contains("query-secret"));
        assert!(!plain.contains("exact-secret"));
        assert!(!plain.contains("private.example"));
    }

    // --- check_tenant ---

    fn make_config(tenants: Vec<&str>) -> TeamsConfig {
        TeamsConfig {
            app_id: "test-app".into(),
            app_secret: "test-secret".into(),
            oauth_endpoint: "https://example.com/token".into(),
            openid_metadata: "https://example.com/openid".into(),
            allowed_tenants: tenants.into_iter().map(|s| s.to_string()).collect(),
            dedupe_ttl_secs: DEFAULT_DEDUPE_TTL_SECS,
            route_ttl_secs: DEFAULT_ROUTE_TTL_SECS,
            max_route_entries: DEFAULT_MAX_ROUTE_ENTRIES,
            reactions_enabled: false,
            inbound_attachments: false,
        }
    }

    fn make_http_test_config(server: &MockServer) -> TeamsConfig {
        let mut config = make_config(vec![]);
        config.oauth_endpoint = format!("{}/token", server.uri());
        config.openid_metadata = format!("{}/openid", server.uri());
        config
    }

    fn make_test_state() -> Arc<crate::AppState> {
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        Arc::new(crate::AppState {
            teams: Some(TeamsAdapter::new(make_config(vec![]))),
            ..crate::AppState::test_default(event_tx)
        })
    }

    fn make_routable_state() -> (
        Arc<crate::AppState>,
        tokio::sync::broadcast::Receiver<String>,
    ) {
        let (event_tx, event_rx) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(crate::AppState {
            teams: Some(TeamsAdapter::new(make_config(vec![]))),
            ..crate::AppState::test_default(event_tx)
        });
        (state, event_rx)
    }

    fn make_reply(command: Option<&str>) -> GatewayReply {
        GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "evt-1".into(),
            platform: "teams".into(),
            channel: ReplyChannel {
                id: "conversation-1".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                text: "reply text".into(),
                attachments: vec![],
            },
            command: command.map(str::to_owned),
            request_id: None,
            quote_message_id: None,
            target_message_id: None,
        }
    }

    async fn accept_test_route(
        adapter: &TeamsAdapter,
        service_url: &str,
        event_id: &str,
        activity_id: &str,
        reply_chain_root_id: Option<&str>,
    ) -> anyhow::Result<()> {
        adapter
            .accept_route_for_test(
                service_url,
                event_id,
                "tenant-1",
                "conversation-1",
                activity_id,
                reply_chain_root_id,
            )
            .await
    }

    fn make_activity_with_tenant(tenant_id: Option<&str>) -> Activity {
        Activity {
            activity_type: "message".into(),
            id: Some("act1".into()),
            timestamp: None,
            service_url: Some("https://smba.trafficmanager.net/".into()),
            channel_id: Some("msteams".into()),
            from: None,
            recipient: None,
            conversation: None,
            text: Some("hello".into()),
            tenant: tenant_id.map(|id| TenantInfo {
                id: Some(id.into()),
            }),
            channel_data: None,
            reply_to_id: None,
            entities: vec![],
            attachments: vec![],
        }
    }

    fn make_attachment_state(
        config: TeamsConfig,
    ) -> (
        Arc<crate::AppState>,
        tokio::sync::broadcast::Receiver<String>,
    ) {
        let (event_tx, event_rx) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(crate::AppState {
            teams: Some(TeamsAdapter::new_for_test(config)),
            ..crate::AppState::test_default(event_tx)
        });
        (state, event_rx)
    }

    fn make_routable_activity(activity_id: &str) -> Activity {
        Activity {
            activity_type: "message".into(),
            id: Some(activity_id.into()),
            timestamp: None,
            service_url: Some("https://smba.trafficmanager.net/emea/".into()),
            channel_id: Some("msteams".into()),
            from: Some(ChannelAccount {
                id: Some("29:user".into()),
                name: Some("Alice".into()),
                aad_object_id: None,
            }),
            recipient: Some(ChannelAccount {
                id: Some("28:bot".into()),
                name: Some("OpenAB".into()),
                aad_object_id: None,
            }),
            conversation: Some(ConversationAccount {
                id: Some("conversation-1".into()),
                conversation_type: Some("channel".into()),
                is_group: Some(true),
                tenant_id: None,
            }),
            text: Some("hello".into()),
            tenant: Some(TenantInfo {
                id: Some("tenant-1".into()),
            }),
            channel_data: Some(ChannelData {
                tenant: None,
                team: Some(ChannelDataEntity {
                    id: Some("team-1".into()),
                }),
                channel: Some(ChannelDataEntity {
                    id: Some("channel-1".into()),
                }),
            }),
            reply_to_id: Some("root-activity".into()),
            entities: vec![],
            attachments: vec![],
        }
    }

    fn make_personal_attachment_activity(
        activity_id: &str,
        service_url: &str,
        attachment: ActivityAttachment,
    ) -> Activity {
        let mut activity = make_routable_activity(activity_id);
        activity.service_url = Some(service_url.into());
        activity.text = None;
        activity.conversation = Some(ConversationAccount {
            id: Some("conversation-1".into()),
            conversation_type: Some("personal".into()),
            is_group: Some(false),
            tenant_id: None,
        });
        activity.channel_data = Some(ChannelData {
            tenant: None,
            team: None,
            channel: None,
        });
        activity.attachments = vec![attachment];
        activity
    }

    fn inline_image_attachment(url: &str) -> ActivityAttachment {
        ActivityAttachment {
            content_type: "image/png".into(),
            content_url: Some(url.into()),
            name: Some("image.png".into()),
            content: None,
        }
    }

    fn personal_file_attachment(
        url: &str,
        filename: &str,
        file_size: Option<u64>,
    ) -> ActivityAttachment {
        ActivityAttachment {
            content_type: TEAMS_FILE_DOWNLOAD_INFO_TYPE.into(),
            content_url: None,
            name: Some(filename.into()),
            content: Some(serde_json::json!({
                "downloadUrl": url,
                "fileSize": file_size,
            })),
        }
    }

    fn tiny_png() -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(2, 2);
        let mut output = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("test PNG encoding");
        output.into_inner()
    }

    #[tokio::test]
    async fn attachment_only_is_ignored_when_disabled_and_publishes_opaque_metadata_when_enabled(
    ) -> anyhow::Result<()> {
        let content_url = "https://smba.trafficmanager.net/emea/v3/attachments/private/views/original?opaque=secret";
        let activity = make_personal_attachment_activity(
            "attachment-disabled",
            "https://smba.trafficmanager.net/emea/",
            inline_image_attachment(content_url),
        );
        let (disabled_state, mut disabled_rx) = make_routable_state();
        assert_eq!(
            accept_message_activity(disabled_state.clone(), activity.clone()).await,
            StatusCode::OK
        );
        assert!(matches!(
            disabled_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        let mut config = make_config(vec![]);
        config.inbound_attachments = true;
        let (enabled_state, mut enabled_rx) = make_attachment_state(config);
        assert_eq!(
            accept_message_activity(enabled_state.clone(), activity).await,
            StatusCode::OK
        );
        let event_json = enabled_rx.recv().await?;
        assert!(!event_json.contains("opaque=secret"));
        assert!(!event_json.contains("/attachments/private/"));
        let event: GatewayEvent = serde_json::from_str(&event_json)?;
        assert!(event.content.text.is_empty());
        assert_eq!(event.content.attachments.len(), 1);
        let reference = event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("opaque reference missing"))?;
        assert!(reference.starts_with("att_"));
        assert!(event.content.attachments[0].data.is_empty());
        assert!(event.content.attachments[0].path.is_none());

        let route = enabled_state
            .teams
            .as_ref()
            .expect("Teams adapter")
            .ingress
            .lock()
            .await
            .route_for_event(&event.event_id, Instant::now())
            .ok_or_else(|| anyhow::anyhow!("attachment route missing"))?;
        assert_eq!(route.attachment_sources.len(), 1);
        assert!(route.attachment_sources.contains_key(reference));
        Ok(())
    }

    #[tokio::test]
    async fn inline_image_materializes_once_with_bot_auth_after_route_acceptance(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "attachment-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let png = tiny_png();
        let _image = Mock::given(method("GET"))
            .and(path("/inline"))
            .and(header("authorization", "Bearer attachment-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_http_test_config(&server);
        config.inbound_attachments = true;
        let (state, mut event_rx) = make_attachment_state(config);
        let activity = make_personal_attachment_activity(
            "inline-materialize",
            &server.uri(),
            inline_image_attachment(&format!("{}/inline?sig=private", server.uri())),
        );
        assert_eq!(
            accept_message_activity(state.clone(), activity).await,
            StatusCode::OK
        );
        let event_json = event_rx.recv().await?;
        assert!(!event_json.contains("sig=private"));
        let event: GatewayEvent = serde_json::from_str(&event_json)?;
        let reference = event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("opaque reference missing"))?;
        let teams = state.teams.as_ref().expect("Teams adapter");
        let attachment = teams
            .materialize_attachment(&event.event_id, &event.channel.id, reference)
            .await?;
        assert!(attachment.status.is_none());
        assert_eq!(attachment.mime_type, "image/jpeg");
        assert!(attachment.reference.is_none());
        assert!(attachment.path.is_none());
        let decoded = attachment.decoded_data()?;
        assert!(!decoded.is_empty());
        assert_eq!(attachment.size, decoded.len() as u64);

        let second = teams
            .materialize_attachment(&event.event_id, &event.channel.id, reference)
            .await
            .expect_err("an opaque reference must be single-use");
        assert_eq!(second.code(), "attachment_reference_not_found");
        Ok(())
    }

    #[tokio::test]
    async fn personal_text_materialization_never_sends_bot_auth_and_rejects_non_utf8(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _text = Mock::given(method("GET"))
            .and(path("/notes"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello teams"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _binary = Mock::given(method("GET"))
            .and(path("/invalid"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes([0xff, 0xfe]))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_http_test_config(&server);
        config.inbound_attachments = true;
        let (state, mut event_rx) = make_attachment_state(config);
        let text_activity = make_personal_attachment_activity(
            "text-materialize",
            &server.uri(),
            personal_file_attachment(
                &format!("{}/notes?sig=private", server.uri()),
                "notes.md",
                Some(11),
            ),
        );
        assert_eq!(
            accept_message_activity(state.clone(), text_activity).await,
            StatusCode::OK
        );
        let event: GatewayEvent = serde_json::from_str(&event_rx.recv().await?)?;
        let reference = event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("text reference missing"))?;
        let teams = state.teams.as_ref().expect("Teams adapter");
        let attachment = teams
            .materialize_attachment(&event.event_id, &event.channel.id, reference)
            .await?;
        assert_eq!(attachment.decoded_data()?, b"hello teams");
        assert_eq!(attachment.mime_type, "text/plain; charset=utf-8");

        let invalid_activity = make_personal_attachment_activity(
            "invalid-text",
            &server.uri(),
            personal_file_attachment(
                &format!("{}/invalid?sig=private", server.uri()),
                "invalid.txt",
                Some(2),
            ),
        );
        assert_eq!(
            accept_message_activity(state.clone(), invalid_activity).await,
            StatusCode::OK
        );
        let invalid_event: GatewayEvent = serde_json::from_str(&event_rx.recv().await?)?;
        let invalid_reference = invalid_event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("invalid text reference missing"))?;
        let rejected = teams
            .materialize_attachment(
                &invalid_event.event_id,
                &invalid_event.channel.id,
                invalid_reference,
            )
            .await?;
        assert!(rejected.data.is_empty());
        assert!(rejected
            .status
            .as_deref()
            .is_some_and(|status| status.starts_with("invalid content:")));

        let requests = server
            .received_requests()
            .await
            .ok_or_else(|| anyhow::anyhow!("request recording is disabled"))?;
        for request in requests
            .iter()
            .filter(|request| matches!(request.url.path(), "/notes" | "/invalid"))
        {
            assert!(!request.headers.contains_key("authorization"));
        }
        assert!(!requests
            .iter()
            .any(|request| request.url.path() == "/token"));
        Ok(())
    }

    #[tokio::test]
    async fn inline_redirect_cannot_forward_bot_auth_to_another_origin() -> anyhow::Result<()> {
        let source = MockServer::start().await;
        let target = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "attachment-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&source)
            .await;
        let _redirect = Mock::given(method("GET"))
            .and(path("/inline"))
            .and(header("authorization", "Bearer attachment-token"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/target", target.uri())),
            )
            .expect(1)
            .mount_as_scoped(&source)
            .await;
        let _target = Mock::given(method("GET"))
            .and(path("/target"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tiny_png()))
            .expect(0)
            .mount_as_scoped(&target)
            .await;

        let mut config = make_http_test_config(&source);
        config.inbound_attachments = true;
        let (state, mut event_rx) = make_attachment_state(config);
        let activity = make_personal_attachment_activity(
            "redirect-image",
            &source.uri(),
            inline_image_attachment(&format!("{}/inline", source.uri())),
        );
        assert_eq!(
            accept_message_activity(state.clone(), activity).await,
            StatusCode::OK
        );
        let event: GatewayEvent = serde_json::from_str(&event_rx.recv().await?)?;
        let reference = event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("redirect reference missing"))?;
        let rejected = state
            .teams
            .as_ref()
            .expect("Teams adapter")
            .materialize_attachment(&event.event_id, &event.channel.id, reference)
            .await?;
        assert!(rejected
            .status
            .as_deref()
            .is_some_and(|status| status.starts_with("security rejected:")));
        Ok(())
    }

    #[tokio::test]
    async fn attachment_metadata_and_download_limits_are_enforced() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _oversized = Mock::given(method("GET"))
            .and(path("/oversized"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                0;
                TEAMS_TEXT_DOWNLOAD_LIMIT
                    as usize
                    + 1
            ]))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let mut config = make_http_test_config(&server);
        config.inbound_attachments = true;
        let (state, mut event_rx) = make_attachment_state(config);
        let activity = make_personal_attachment_activity(
            "oversized-text",
            &server.uri(),
            personal_file_attachment(
                &format!("{}/oversized?sig=private", server.uri()),
                "notes.txt",
                None,
            ),
        );
        assert_eq!(
            accept_message_activity(state.clone(), activity).await,
            StatusCode::OK
        );
        let event: GatewayEvent = serde_json::from_str(&event_rx.recv().await?)?;
        let reference = event.content.attachments[0]
            .reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("oversized reference missing"))?;
        let rejected = state
            .teams
            .as_ref()
            .expect("Teams adapter")
            .materialize_attachment(&event.event_id, &event.channel.id, reference)
            .await?;
        let rejection = rejected
            .status
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("oversized attachment was not rejected"))?;
        assert!(rejection.starts_with("size exceeded:"), "{rejection}");

        let teams = state.teams.as_ref().expect("Teams adapter");
        let service = reqwest::Url::parse(&server.uri())?;
        let mut many = make_personal_attachment_activity(
            "many-attachments",
            &server.uri(),
            inline_image_attachment(&format!("{}/image-0", server.uri())),
        );
        many.attachments = (0..12)
            .map(|index| ActivityAttachment {
                content_type: "image/png".into(),
                content_url: Some(format!("{}/image-{index}", server.uri())),
                name: Some(format!("{}-{index}.png", "a".repeat(240))),
                content: None,
            })
            .collect();
        let prepared = prepare_attachment_metadata(teams, &many, &service, "personal");
        assert_eq!(prepared.metadata.len(), TEAMS_ATTACHMENT_METADATA_LIMIT);
        assert_eq!(prepared.sources.len(), TEAMS_ATTACHMENT_METADATA_LIMIT);
        assert!(prepared
            .metadata
            .iter()
            .all(|attachment| attachment.filename.chars().count() <= TEAMS_FILENAME_LIMIT));
        Ok(())
    }

    #[test]
    fn attachment_url_and_scope_policy_is_fail_closed() -> anyhow::Result<()> {
        let service = reqwest::Url::parse("https://smba.trafficmanager.net/emea/")?;
        assert!(validate_inline_attachment_url(
            "https://smba.trafficmanager.net/emea/attachment?sig=opaque",
            &service,
            false,
        )
        .is_ok());
        assert!(
            validate_inline_attachment_url("https://evil.example/attachment", &service, false,)
                .is_err()
        );
        assert!(validate_file_attachment_url(
            "https://tenant.sharepoint.com/file?sig=opaque",
            false,
        )
        .is_ok());
        for unsafe_url in [
            "http://tenant.sharepoint.com/file",
            "https://127.0.0.1/file",
            "https://evilsharepoint.com/file",
            "https://tenant.sharepoint.com:444/file",
            "https://user@tenant.sharepoint.com/file",
            "https://tenant.sharepoint.com/file#fragment",
        ] {
            assert!(validate_file_attachment_url(unsafe_url, false).is_err());
        }

        let mut config = make_config(vec![]);
        config.inbound_attachments = true;
        let adapter = TeamsAdapter::new(config);
        let group_attachment = personal_file_attachment(
            "https://tenant.sharepoint.com/file?sig=opaque",
            "notes.txt",
            Some(5),
        );
        let activity =
            make_personal_attachment_activity("group-file", service.as_str(), group_attachment);
        let prepared = prepare_attachment_metadata(&adapter, &activity, &service, "groupChat");
        assert!(prepared.sources.is_empty());
        assert_eq!(prepared.metadata.len(), 1);
        assert!(prepared.metadata[0]
            .status
            .as_deref()
            .is_some_and(|status| status.starts_with("unsupported format:")));

        let mut missing_scope = activity;
        missing_scope
            .conversation
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("test activity is missing its conversation"))?
            .conversation_type = None;
        let prepared =
            prepare_attachment_metadata(&adapter, &missing_scope, &service, "personal");
        assert!(prepared.sources.is_empty());
        assert!(prepared.metadata[0]
            .status
            .as_deref()
            .is_some_and(|status| status.starts_with("unsupported format:")));
        Ok(())
    }

    // --- webhook body limit ---

    #[tokio::test]
    async fn webhook_rejects_oversized_body_before_auth() {
        let status = webhook(
            State(make_test_state()),
            HeaderMap::new(),
            "x".repeat(WEBHOOK_BODY_LIMIT + 1),
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn webhook_allows_body_at_limit_to_reach_auth() {
        let status = webhook(
            State(make_test_state()),
            HeaderMap::new(),
            "x".repeat(WEBHOOK_BODY_LIMIT),
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_rejects_missing_route_fields_before_jwt_fetch() -> anyhow::Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer invalid".parse()?);
        let status = webhook(
            State(make_test_state()),
            headers,
            r#"{"type":"message","text":"hello"}"#.into(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn post_auth_requires_all_route_and_identity_fields() -> anyhow::Result<()> {
        let (state, _event_rx) = make_routable_state();

        let mut cases = Vec::new();
        let mut missing_channel_id = make_routable_activity("missing-channel-id");
        missing_channel_id.channel_id = None;
        cases.push(missing_channel_id);
        let mut missing_tenant = make_routable_activity("missing-tenant");
        missing_tenant.tenant = None;
        cases.push(missing_tenant);
        let mut missing_conversation = make_routable_activity("missing-conversation");
        let Some(conversation) = missing_conversation.conversation.as_mut() else {
            anyhow::bail!("test activity must include a conversation")
        };
        conversation.id = None;
        cases.push(missing_conversation);
        let mut missing_activity = make_routable_activity("missing-activity");
        missing_activity.id = None;
        cases.push(missing_activity);
        let mut missing_sender = make_routable_activity("missing-sender");
        let Some(sender) = missing_sender.from.as_mut() else {
            anyhow::bail!("test activity must include a sender")
        };
        sender.id = None;
        cases.push(missing_sender);
        let mut missing_service_url = make_routable_activity("missing-service-url");
        missing_service_url.service_url = None;
        cases.push(missing_service_url);

        for activity in cases {
            assert_eq!(
                accept_message_activity(state.clone(), activity).await,
                StatusCode::BAD_REQUEST
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn no_consumer_returns_503_without_leaving_a_dedupe_tombstone() -> anyhow::Result<()> {
        let (event_tx, event_rx) = tokio::sync::broadcast::channel(16);
        drop(event_rx);
        let state = Arc::new(crate::AppState {
            teams: Some(TeamsAdapter::new(make_config(vec![]))),
            ..crate::AppState::test_default(event_tx)
        });
        let activity = make_routable_activity("retryable-activity");

        assert_eq!(
            accept_message_activity(state.clone(), activity.clone()).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut event_rx = state.event_tx.subscribe();
        assert_eq!(
            accept_message_activity(state.clone(), activity).await,
            StatusCode::OK
        );
        let event_json = event_rx.recv().await?;
        let event: GatewayEvent = serde_json::from_str(&event_json)?;
        let teams = state
            .teams
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("test state must include Teams"))?;
        let route = teams
            .ingress
            .lock()
            .await
            .route_for_event(&event.event_id, Instant::now())
            .ok_or_else(|| anyhow::anyhow!("successful retry should commit an ingress route"))?;
        assert_eq!(route.tenant_id, "tenant-1");
        assert_eq!(route.conversation_id, "conversation-1");
        assert_eq!(route.inbound_activity_id, "retryable-activity");
        assert_eq!(route.reply_chain_root_id.as_deref(), Some("root-activity"));
        assert_eq!(route.team_id.as_deref(), Some("team-1"));
        assert_eq!(route.channel_id.as_deref(), Some("channel-1"));
        Ok(())
    }

    #[tokio::test]
    async fn accepted_duplicate_publishes_exactly_one_gateway_event() -> anyhow::Result<()> {
        let (state, mut event_rx) = make_routable_state();
        let activity = make_routable_activity("duplicate-activity");

        assert_eq!(
            accept_message_activity(state.clone(), activity.clone()).await,
            StatusCode::OK
        );
        assert_eq!(
            accept_message_activity(state.clone(), activity).await,
            StatusCode::OK
        );
        event_rx.recv().await?;
        assert!(matches!(
            event_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_duplicate_waiters_share_one_publish_result() -> anyhow::Result<()> {
        let (state, mut event_rx) = make_routable_state();
        let activity = make_routable_activity("concurrent-activity");
        let mut tasks = Vec::new();
        for _ in 0..16 {
            tasks.push(tokio::spawn(accept_message_activity(
                state.clone(),
                activity.clone(),
            )));
        }
        for task in tasks {
            assert_eq!(task.await?, StatusCode::OK);
        }

        event_rx.recv().await?;
        assert!(matches!(
            event_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        Ok(())
    }

    #[test]
    fn tenant_allowed_when_list_empty() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let activity = make_activity_with_tenant(Some("any-tenant"));
        assert!(adapter.check_tenant(&activity));
    }

    #[test]
    fn tenant_allowed_when_in_list() {
        let adapter = TeamsAdapter::new(make_config(vec!["tenant-a", "tenant-b"]));
        let activity = make_activity_with_tenant(Some("tenant-b"));
        assert!(adapter.check_tenant(&activity));
    }

    #[test]
    fn tenant_rejected_when_not_in_list() {
        let adapter = TeamsAdapter::new(make_config(vec!["tenant-a"]));
        let activity = make_activity_with_tenant(Some("tenant-x"));
        assert!(!adapter.check_tenant(&activity));
    }

    #[test]
    fn tenant_rejected_when_no_tenant_info() {
        let adapter = TeamsAdapter::new(make_config(vec!["tenant-a"]));
        let activity = make_activity_with_tenant(None);
        assert!(!adapter.check_tenant(&activity));
    }

    #[test]
    fn tenant_allowed_when_no_tenant_and_empty_list() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let activity = make_activity_with_tenant(None);
        assert!(adapter.check_tenant(&activity));
    }

    // --- resolved_tenant_id ---

    #[test]
    fn resolved_tenant_falls_back_to_channel_data() -> anyhow::Result<()> {
        // Teams personal/channel webhooks put tenant in channelData, not top-level
        let json = r#"{
            "type": "message",
            "channelData": {"tenant": {"id": "from-channel-data"}}
        }"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.resolved_tenant_id(), Some("from-channel-data"));
        Ok(())
    }

    #[test]
    fn resolved_tenant_prefers_top_level_over_channel_data() -> anyhow::Result<()> {
        let json = r#"{
            "type": "message",
            "tenant": {"id": "top-level"},
            "channelData": {"tenant": {"id": "from-channel-data"}}
        }"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.resolved_tenant_id(), Some("top-level"));
        Ok(())
    }

    #[test]
    fn resolved_tenant_falls_back_to_conversation_tenant_id() -> anyhow::Result<()> {
        let json = r#"{
            "type": "message",
            "conversation": {"id": "c1", "tenantId": "from-conversation"}
        }"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.resolved_tenant_id(), Some("from-conversation"));
        Ok(())
    }

    #[test]
    fn resolved_tenant_returns_none_when_absent() -> anyhow::Result<()> {
        let json = r#"{"type": "message"}"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.resolved_tenant_id(), None);
        Ok(())
    }

    // --- validate_jwt error paths ---

    #[tokio::test]
    async fn jwt_rejects_missing_bearer_prefix() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let activity = make_activity_with_tenant(Some("t1"));
        let result = adapter.validate_jwt("NotBearer xyz", &activity).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Bearer"));
    }

    #[tokio::test]
    async fn jwt_rejects_empty_bearer() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let activity = make_activity_with_tenant(Some("t1"));
        let result = adapter.validate_jwt("Bearer ", &activity).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn jwt_rejects_garbage_token() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let activity = make_activity_with_tenant(Some("t1"));
        let result = adapter
            .validate_jwt("Bearer not.a.valid.jwt", &activity)
            .await;
        assert!(result.is_err());
    }

    // --- Activity deserialization ---

    #[test]
    fn deserialize_minimal_activity() -> anyhow::Result<()> {
        let json = r#"{"type": "message"}"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.activity_type, "message");
        assert!(activity.text.is_none());
        assert!(activity.from.is_none());
        Ok(())
    }

    #[test]
    fn deserialize_full_activity() -> anyhow::Result<()> {
        let json = r#"{
            "type": "message",
            "id": "act123",
            "serviceUrl": "https://smba.trafficmanager.net/",
            "channelId": "msteams",
            "from": {"id": "user1", "name": "Alice", "aadObjectId": "aad-123"},
            "recipient": {"id": "bot1", "name": "OpenAB"},
            "conversation": {"id": "conv1", "conversationType": "personal", "isGroup": false},
            "text": "hello bot",
            "tenant": {"id": "tenant-abc"},
            "replyToId": "root-activity",
            "channelData": {
                "team": {"id": "team-abc"},
                "channel": {"id": "channel-abc"}
            },
            "entities": [
                {"type": "mention", "mentioned": {"id": "bot1", "name": "OpenAB"}, "text": "<at>OpenAB</at>"},
                {"type": "mention", "mentioned": {"id": "user2", "name": "Bob"}, "text": "<at>Bob</at>"},
                {"type": "clientInfo"}
            ]
        }"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.activity_type, "message");
        assert_eq!(activity.text.as_deref(), Some("hello bot"));
        assert_eq!(
            activity
                .from
                .as_ref()
                .and_then(|sender| sender.name.as_deref()),
            Some("Alice")
        );
        assert_eq!(
            activity
                .tenant
                .as_ref()
                .and_then(|tenant| tenant.id.as_deref()),
            Some("tenant-abc")
        );
        assert_eq!(activity.reply_to_id.as_deref(), Some("root-activity"));
        assert_eq!(
            activity
                .recipient
                .as_ref()
                .and_then(|recipient| recipient.id.as_deref()),
            Some("bot1")
        );
        let channel_data = activity
            .channel_data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("channelData should deserialize"))?;
        assert_eq!(
            channel_data
                .team
                .as_ref()
                .and_then(|team| team.id.as_deref()),
            Some("team-abc")
        );
        assert_eq!(
            channel_data
                .channel
                .as_ref()
                .and_then(|channel| channel.id.as_deref()),
            Some("channel-abc")
        );
        let (mention_ids, mention_entities) = activity.mention_info();
        assert_eq!(mention_ids, vec!["bot1", "user2"]);
        assert_eq!(
            mention_entities,
            vec![
                MentionInfo {
                    id: "bot1".into(),
                    text: "<at>OpenAB</at>".into(),
                },
                MentionInfo {
                    id: "user2".into(),
                    text: "<at>Bob</at>".into(),
                },
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn accepted_event_carries_typed_scope_recipient_and_mentions() -> anyhow::Result<()> {
        let (state, mut event_rx) = make_routable_state();
        let mut activity = make_routable_activity("typed-activity");
        activity.text = Some("<at>OpenAB</at> ask <at>Bob</at>".into());
        activity.entities = vec![
            ActivityEntity {
                entity_type: "mention".into(),
                mentioned: Some(ChannelAccount {
                    id: Some("28:bot".into()),
                    name: Some("OpenAB".into()),
                    aad_object_id: None,
                }),
                text: Some("<at>OpenAB</at>".into()),
            },
            ActivityEntity {
                entity_type: "mention".into(),
                mentioned: Some(ChannelAccount {
                    id: Some("29:bob".into()),
                    name: Some("Bob".into()),
                    aad_object_id: None,
                }),
                text: Some("<at>Bob</at>".into()),
            },
            ActivityEntity {
                entity_type: "mention".into(),
                mentioned: Some(ChannelAccount {
                    id: Some("28:bot".into()),
                    name: Some("OpenAB".into()),
                    aad_object_id: None,
                }),
                text: Some(String::new()),
            },
            ActivityEntity {
                entity_type: "clientInfo".into(),
                mentioned: None,
                text: None,
            },
        ];

        assert_eq!(
            accept_message_activity(state, activity).await,
            StatusCode::OK
        );
        let event: GatewayEvent = serde_json::from_str(&event_rx.recv().await?)?;
        assert_eq!(event.channel.id, "conversation-1");
        assert_eq!(event.channel.channel_type, "channel");
        assert_eq!(event.content.text, "<at>OpenAB</at> ask <at>Bob</at>");
        assert_eq!(event.mentions, vec!["28:bot", "29:bob"]);
        assert_eq!(
            event.recipient,
            Some(RecipientInfo {
                id: "28:bot".into(),
                name: "OpenAB".into(),
            })
        );
        assert_eq!(
            event.scope,
            Some(GatewayScope {
                tenant_id: Some("tenant-1".into()),
                team_id: Some("team-1".into()),
                channel_id: Some("channel-1".into()),
                conversation_type: "channel".into(),
                trust_scope_id: "teams:tenant-1:team:team-1:channel:channel-1".into(),
                is_dm: false,
            })
        );
        assert_eq!(
            event.mention_entities,
            vec![
                MentionInfo {
                    id: "28:bot".into(),
                    text: "<at>OpenAB</at>".into(),
                },
                MentionInfo {
                    id: "29:bob".into(),
                    text: "<at>Bob</at>".into(),
                },
                MentionInfo {
                    id: "28:bot".into(),
                    text: String::new(),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn scope_derivation_canonicalizes_known_conversation_types() -> anyhow::Result<()> {
        let mut activity = make_routable_activity("scope-activity");
        let conversation = activity
            .conversation
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("test activity must include conversation"))?;

        conversation.conversation_type = Some("GROUPCHAT".into());
        activity.channel_data = None;
        let kind = canonical_conversation_type("GROUPCHAT");
        let group = activity.gateway_scope("tenant-1", "conversation-1", &kind);
        assert_eq!(group.conversation_type, "groupChat");
        assert!(!group.is_dm);
        assert_eq!(
            group.trust_scope_id,
            "teams:tenant-1:group-chat:conversation-1"
        );

        let kind = canonical_conversation_type("Personal");
        let personal = activity.gateway_scope("tenant-1", "conversation-1", &kind);
        assert_eq!(personal.conversation_type, "personal");
        assert!(personal.is_dm);
        assert_eq!(
            personal.trust_scope_id,
            "teams:tenant-1:personal:conversation-1"
        );

        let kind = canonical_conversation_type("meeting");
        let unknown = activity.gateway_scope("tenant-1", "conversation-1", &kind);
        assert_eq!(unknown.conversation_type, "meeting");
        assert!(!unknown.is_dm);
        assert!(unknown.trust_scope_id.contains(":unknown:meeting:"));
        Ok(())
    }

    #[test]
    fn deserialize_non_message_activity() -> anyhow::Result<()> {
        let json = r#"{"type": "conversationUpdate"}"#;
        let activity: Activity = serde_json::from_str(json)?;
        assert_eq!(activity.activity_type, "conversationUpdate");
        Ok(())
    }

    #[test]
    fn deserialize_invalid_json_fails() {
        let result = serde_json::from_str::<Activity>("not json");
        assert!(result.is_err());
    }

    // --- transport concurrency and HTTP policy ---

    #[tokio::test]
    async fn concurrent_oauth_callers_share_one_refresh() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(20))
                    .set_body_json(serde_json::json!({
                        "access_token": "singleflight-token",
                        "expires_in": 3600
                    })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let (first, second) = tokio::join!(adapter.get_token(), adapter.get_token());
        assert_eq!(first?, "singleflight-token");
        assert_eq!(second?, "singleflight-token");
        Ok(())
    }

    #[tokio::test]
    async fn oauth_error_redacts_configured_app_secret() {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string("rejected test-secret at https://sensitive.example/token"),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let error = adapter.get_token().await.unwrap_err().to_string();
        assert!(error.contains("401"));
        assert!(!error.contains("test-secret"));
        assert!(!error.contains("sensitive.example"));
    }

    #[tokio::test]
    async fn concurrent_jwks_callers_share_metadata_and_key_fetches() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _metadata = Mock::given(method("GET"))
            .and(path("/openid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jwks_uri": format!("{}/keys", server.uri())
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _keys = Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(20))
                    .set_body_json(serde_json::json!({
                        "keys": [{
                            "kid": "key-1",
                            "n": "modulus",
                            "e": "AQAB",
                            "kty": "RSA",
                            "endorsements": ["msteams"]
                        }]
                    })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let (first, second) = tokio::join!(adapter.get_jwks(), adapter.get_jwks());
        assert_eq!(first?.keys.len(), 1);
        assert_eq!(second?.keys.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn unsafe_service_url_is_rejected_before_oauth() {
        let server = MockServer::start().await;
        let _no_token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new(make_http_test_config(&server));

        let error = adapter
            .send_activity("http://127.0.0.1/", "conversation-1", "hello", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTPS"));
    }

    #[tokio::test]
    async fn connector_success_without_activity_id_is_unknown() {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _activity = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let outcome = adapter
            .send_activity_outcome(&server.uri(), "conversation-1", "hello", None)
            .await;
        assert_eq!(
            outcome,
            WriteOutcome::Unknown {
                code: "missing_activity_id".into(),
                message: "Bot Framework send response missing activity id".into(),
            }
        );
    }

    #[tokio::test]
    async fn connector_does_not_follow_cross_origin_redirects() {
        let source = MockServer::start().await;
        let target = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&source)
            .await;
        let _redirect = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/captured", target.uri())),
            )
            .expect(1)
            .mount_as_scoped(&source)
            .await;
        let _not_reached = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&target)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&source));

        let error = adapter
            .send_activity(&source.uri(), "conversation-1", "hello", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("307"));
    }

    #[tokio::test]
    async fn connector_error_body_is_bounded_and_redacted() {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let sensitive_body = format!(
            "access_token:leaked-token exact bearer test-token https://sensitive.example/path {}",
            "x".repeat(TEAMS_ERROR_BODY_LIMIT * 2)
        );
        let _activity = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(ResponseTemplate::new(500).set_body_string(sensitive_body))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let outcome = adapter
            .send_activity_outcome(&server.uri(), "conversation-1", "hello", None)
            .await;
        let WriteOutcome::Unknown { code, message } = outcome else {
            panic!("HTTP 500 must preserve ambiguous delivery")
        };
        assert_eq!(code, "connector_server_error");
        assert!(message.contains("500"));
        assert!(message.contains("[truncated]"));
        assert!(!message.contains("leaked-token"));
        assert!(!message.contains("test-token"));
        assert!(!message.contains("sensitive.example"));
        assert!(message.len() <= TEAMS_ERROR_BODY_LIMIT + 256);
    }

    #[tokio::test]
    async fn connector_request_timeout_hides_service_url() {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _activity = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(250)))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test_with_timeout(
            make_http_test_config(&server),
            Duration::from_millis(75),
        );

        let outcome = adapter
            .send_activity_outcome(&server.uri(), "conversation-1", "hello", None)
            .await;
        let WriteOutcome::Unknown { code, message } = outcome else {
            panic!("POST timeout must preserve ambiguous delivery")
        };
        assert_eq!(code, "request_timeout");
        assert!(message.contains("timed out"));
        assert!(!message.contains(&server.uri()));
    }

    #[tokio::test]
    async fn connector_classifies_rejection_and_retry_after() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _rejected = Mock::given(method("POST"))
            .and(path("/v3/conversations/rejected/activities"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad activity"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _too_large = Mock::given(method("POST"))
            .and(path("/v3/conversations/too-large/activities"))
            .respond_with(ResponseTemplate::new(413).set_body_string("message too large"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _rate_limited = Mock::given(method("POST"))
            .and(path("/v3/conversations/rate-limited/activities"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "2")
                    .set_body_string("slow down"),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        let rejected = adapter
            .send_activity_outcome(&server.uri(), "rejected", "hello", None)
            .await;
        assert!(matches!(
            rejected,
            WriteOutcome::Rejected {
                ref code,
                retry_after_ms: None,
                ..
            } if code == "connector_rejected"
        ));

        let too_large = adapter
            .send_activity_outcome(&server.uri(), "too-large", "hello", None)
            .await;
        assert!(matches!(
            too_large,
            WriteOutcome::Rejected {
                ref code,
                retry_after_ms: None,
                ..
            } if code == "message_too_large"
        ));

        let rate_limited = adapter
            .send_activity_outcome(&server.uri(), "rate-limited", "hello", None)
            .await;
        assert!(matches!(
            rate_limited,
            WriteOutcome::Rejected {
                ref code,
                retry_after_ms: Some(2000),
                ..
            } if code == "rate_limited"
        ));
        Ok(())
    }

    // --- reply command dispatch ---

    #[tokio::test]
    async fn unsupported_commands_never_fall_through_to_send_activity() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _no_post = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_config(vec![]);
        config.oauth_endpoint = format!("{}/token", server.uri());
        let adapter = TeamsAdapter::new_for_test(config);

        for command in ["add_reaction", "remove_reaction"] {
            let outcome = handle_reply(&make_reply(Some(command)), &adapter).await;
            assert_eq!(
                outcome,
                WriteOutcome::Delivered { message_id: None },
                "reaction command {command} should be a no-op"
            );
        }

        for command in ["create_topic", "future_unknown_command"] {
            let outcome = handle_reply(&make_reply(Some(command)), &adapter).await;
            assert!(
                matches!(
                    outcome,
                    WriteOutcome::Rejected { ref code, ref message, .. }
                        if code == "unsupported_command" && message.contains(command)
                ),
                "outcome should identify unsupported command {command}: {outcome:?}"
            );
        }

        for command in ["edit_message", "delete_message"] {
            let outcome = handle_reply(&make_reply(Some(command)), &adapter).await;
            assert!(
                matches!(
                    outcome,
                    WriteOutcome::Rejected { ref code, .. } if code == "message_not_owned"
                ),
                "unowned command target must be rejected before HTTP: {outcome:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn enabled_reactions_add_remove_and_accept_legacy_targets() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _add = Mock::given(method("PUT"))
            .and(path(
                "/v3/conversations/conversation-1/activities/inbound-1/reactions/1f440_eyes",
            ))
            .and(header("content-length", "0"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _remove = Mock::given(method("DELETE"))
            .and(path(
                "/v3/conversations/conversation-1/activities/inbound-1/reactions/1f440_eyes",
            ))
            .and(header("content-length", "0"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _legacy_add = Mock::given(method("PUT"))
            .and(path(
                "/v3/conversations/conversation-1/activities/inbound-1/reactions/heart",
            ))
            .and(header("content-length", "0"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = attempts.clone();
        let _rate_limited = Mock::given(method("PUT"))
            .and(path(
                "/v3/conversations/conversation-1/activities/inbound-1/reactions/think",
            ))
            .and(header("content-length", "0"))
            .respond_with(move |_request: &wiremock::Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).insert_header("retry-after", "0")
                } else {
                    ResponseTemplate::new(204)
                }
            })
            .expect(2)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_http_test_config(&server);
        config.reactions_enabled = true;
        let adapter = TeamsAdapter::new_for_test(config);
        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;

        for command in ["add_reaction", "remove_reaction"] {
            let mut reply = make_reply(Some(command));
            reply.target_message_id = Some("inbound-1".into());
            reply.content.text = "👀".into();
            assert_eq!(
                handle_reply(&reply, &adapter).await,
                WriteOutcome::Delivered { message_id: None }
            );
        }

        let mut legacy = make_reply(Some("add_reaction"));
        legacy.reply_to = "inbound-1".into();
        legacy.content.text = "❤️".into();
        assert_eq!(
            handle_reply(&legacy, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );

        let mut rate_limited = make_reply(Some("add_reaction"));
        rate_limited.target_message_id = Some("inbound-1".into());
        rate_limited.content.text = "🤔".into();
        assert_eq!(
            handle_reply(&rate_limited, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        let mut unknown = make_reply(Some("add_reaction"));
        unknown.target_message_id = Some("untrusted-activity".into());
        unknown.content.text = "👀".into();
        assert!(matches!(
            handle_reply(&unknown, &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "reaction_target_not_known"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn commandless_reply_still_sends_one_activity() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _activity = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .and(body_json(serde_json::json!({
                "type": "message",
                "from": { "id": "test-app" },
                "text": "reply text",
                "textFormat": "markdown"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "activity-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_config(vec![]);
        config.oauth_endpoint = format!("{}/token", server.uri());
        let adapter = TeamsAdapter::new_for_test(config);
        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;

        let outcome = handle_reply(&make_reply(None), &adapter).await;
        assert_eq!(
            outcome,
            WriteOutcome::Delivered {
                message_id: Some("activity-1".into())
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn bot_owned_edit_and_delete_use_structured_target_and_legacy_fallback(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _send = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "bot-activity-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _edit = Mock::given(method("PUT"))
            .and(path(
                "/v3/conversations/conversation-1/activities/bot-activity-1",
            ))
            .and(body_json(serde_json::json!({
                "type": "message",
                "from": { "id": "test-app" },
                "text": "updated text",
                "textFormat": "markdown"
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount_as_scoped(&server)
            .await;
        let _delete = Mock::given(method("DELETE"))
            .and(path(
                "/v3/conversations/conversation-1/activities/bot-activity-1",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));
        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;
        assert!(matches!(
            handle_reply(&make_reply(None), &adapter).await,
            WriteOutcome::Delivered { ref message_id }
                if message_id.as_deref() == Some("bot-activity-1")
        ));

        let mut structured_edit = make_reply(Some("edit_message"));
        structured_edit.content.text = "updated text".into();
        structured_edit.target_message_id = Some("bot-activity-1".into());
        assert_eq!(
            handle_reply(&structured_edit, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );

        let mut legacy_edit = make_reply(Some("edit_message"));
        legacy_edit.reply_to = "bot-activity-1".into();
        legacy_edit.content.text = "updated text".into();
        assert_eq!(
            handle_reply(&legacy_edit, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );

        let mut delete = make_reply(Some("delete_message"));
        delete.target_message_id = Some("bot-activity-1".into());
        assert_eq!(
            handle_reply(&delete, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );
        assert!(matches!(
            handle_reply(&structured_edit, &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "message_not_owned"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn unknown_delete_outcome_preserves_ownership_for_later_reconciliation(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _send = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "bot-activity-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _delete = Mock::given(method("DELETE"))
            .and(path(
                "/v3/conversations/conversation-1/activities/bot-activity-1",
            ))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _edit = Mock::given(method("PUT"))
            .and(path(
                "/v3/conversations/conversation-1/activities/bot-activity-1",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));
        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;
        assert!(matches!(
            handle_reply(&make_reply(None), &adapter).await,
            WriteOutcome::Delivered { .. }
        ));

        let mut delete = make_reply(Some("delete_message"));
        delete.target_message_id = Some("bot-activity-1".into());
        assert!(matches!(
            handle_reply(&delete, &adapter).await,
            WriteOutcome::Unknown { ref code, .. } if code == "connector_server_error"
        ));

        let mut edit = make_reply(Some("edit_message"));
        edit.target_message_id = Some("bot-activity-1".into());
        assert_eq!(
            handle_reply(&edit, &adapter).await,
            WriteOutcome::Delivered { message_id: None }
        );
        Ok(())
    }

    #[tokio::test]
    async fn inbound_or_cross_conversation_mutation_is_rejected_before_http() -> anyhow::Result<()>
    {
        let server = MockServer::start().await;
        let _no_http = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let _no_put = Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));
        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;

        let mut inbound_target = make_reply(Some("edit_message"));
        inbound_target.target_message_id = Some("inbound-1".into());
        assert!(matches!(
            handle_reply(&inbound_target, &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "message_not_owned"
        ));

        let mut wrong_conversation = inbound_target;
        wrong_conversation.channel.id = "conversation-2".into();
        assert!(matches!(
            handle_reply(&wrong_conversation, &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "target_scope_mismatch"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mutation_outcomes_and_bounded_rate_limit_retry_are_explicit() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _server_error = Mock::given(method("PUT"))
            .and(path("/v3/conversations/server-error/activities/bot-1"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _rejected = Mock::given(method("DELETE"))
            .and(path("/v3/conversations/rejected/activities/bot-1"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _long_rate_limit = Mock::given(method("DELETE"))
            .and(path("/v3/conversations/long-rate-limit/activities/bot-1"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let responder_attempts = attempts.clone();
        let _rate_limited = Mock::given(method("PUT"))
            .and(path("/v3/conversations/rate-limited/activities/bot-1"))
            .respond_with(move |_request: &wiremock::Request| {
                if responder_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(429).insert_header("retry-after", "0")
                } else {
                    ResponseTemplate::new(200)
                }
            })
            .expect(2)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        assert!(matches!(
            adapter
                .update_activity_outcome(&server.uri(), "server-error", "bot-1", "updated")
                .await,
            WriteOutcome::Unknown { ref code, .. } if code == "connector_server_error"
        ));
        assert!(matches!(
            adapter
                .delete_activity_outcome(&server.uri(), "rejected", "bot-1")
                .await,
            WriteOutcome::Rejected { ref code, .. } if code == "authorization_rejected"
        ));
        assert!(matches!(
            adapter
                .delete_activity_outcome(&server.uri(), "long-rate-limit", "bot-1")
                .await,
            WriteOutcome::Rejected {
                ref code,
                retry_after_ms: Some(2000),
                ..
            } if code == "rate_limited"
        ));
        assert_eq!(
            adapter
                .update_activity_outcome(&server.uri(), "rate-limited", "bot-1", "updated")
                .await,
            WriteOutcome::Delivered { message_id: None }
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn mutation_timeout_is_unknown_and_not_retried() {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _update = Mock::given(method("PUT"))
            .and(path("/v3/conversations/conversation-1/activities/bot-1"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(250)))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test_with_timeout(
            make_http_test_config(&server),
            Duration::from_millis(75),
        );

        assert!(matches!(
            adapter
                .update_activity_outcome(&server.uri(), "conversation-1", "bot-1", "updated")
                .await,
            WriteOutcome::Unknown { ref code, .. } if code == "request_timeout"
        ));
    }

    #[tokio::test]
    async fn conversation_write_shards_serialize_same_scope_without_blocking_others(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));
        adapter
            .accept_route_for_test(
                &server.uri(),
                "event-1",
                "tenant-1",
                "conversation-1",
                "inbound-1",
                None,
            )
            .await?;
        let route_one = adapter
            .ingress
            .lock()
            .await
            .route_for_event("event-1", Instant::now())
            .ok_or_else(|| anyhow::anyhow!("first test route missing"))?;

        let mut other_conversation = 2usize;
        let route_two = loop {
            let conversation = format!("conversation-{other_conversation}");
            let event = format!("event-{other_conversation}");
            let inbound = format!("inbound-{other_conversation}");
            adapter
                .accept_route_for_test(
                    &server.uri(),
                    &event,
                    "tenant-1",
                    &conversation,
                    &inbound,
                    None,
                )
                .await?;
            let candidate = adapter
                .ingress
                .lock()
                .await
                .route_for_event(&event, Instant::now())
                .ok_or_else(|| anyhow::anyhow!("second test route missing"))?;
            if TeamsAdapter::conversation_write_shard(&candidate)
                != TeamsAdapter::conversation_write_shard(&route_one)
            {
                break candidate;
            }
            other_conversation += 1;
            if other_conversation > TEAMS_WRITE_SHARDS * 4 {
                anyhow::bail!("failed to find a distinct conversation write shard");
            }
        };

        let first_guard = adapter.lock_conversation(&route_one).await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                adapter.lock_conversation(&route_one)
            )
            .await
            .is_err(),
            "same conversation must wait for the active write"
        );
        let other_guard = tokio::time::timeout(
            Duration::from_millis(20),
            adapter.lock_conversation(&route_two),
        )
        .await
        .map_err(|_| anyhow::anyhow!("different conversation was unnecessarily serialized"))?;
        drop(other_guard);
        drop(first_guard);
        Ok(())
    }

    #[tokio::test]
    async fn explicit_quote_is_scoped_and_unknown_target_falls_back_to_plain_send(
    ) -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _token = Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-token",
                "expires_in": 3600
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _quoted = Mock::given(method("POST"))
            .and(path(
                "/v3/conversations/conversation-1/activities/inbound-1",
            ))
            .and(body_json(serde_json::json!({
                "type": "message",
                "from": { "id": "test-app" },
                "text": "reply text",
                "textFormat": "markdown",
                "replyToId": "inbound-1"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "quoted-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _plain = Mock::given(method("POST"))
            .and(path("/v3/conversations/conversation-1/activities"))
            .and(body_json(serde_json::json!({
                "type": "message",
                "from": { "id": "test-app" },
                "text": "reply text",
                "textFormat": "markdown"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "plain-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));
        accept_test_route(
            &adapter,
            &server.uri(),
            "evt-1",
            "inbound-1",
            Some("root-1"),
        )
        .await?;

        let mut quoted_reply = make_reply(None);
        quoted_reply.quote_message_id = Some("inbound-1".into());
        assert_eq!(
            handle_reply(&quoted_reply, &adapter).await,
            WriteOutcome::Delivered {
                message_id: Some("quoted-1".into())
            }
        );

        let mut unknown_quote = make_reply(None);
        unknown_quote.quote_message_id = Some("activity-from-another-scope".into());
        assert_eq!(
            handle_reply(&unknown_quote, &adapter).await,
            WriteOutcome::Delivered {
                message_id: Some("plain-1".into())
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_or_cross_conversation_route_is_rejected_before_http() -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let _no_http = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let adapter = TeamsAdapter::new_for_test(make_http_test_config(&server));

        assert!(matches!(
            handle_reply(&make_reply(None), &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "route_not_found"
        ));

        accept_test_route(&adapter, &server.uri(), "evt-1", "inbound-1", None).await?;
        let mut mismatched = make_reply(None);
        mismatched.channel.id = "conversation-2".into();
        assert!(matches!(
            handle_reply(&mismatched, &adapter).await,
            WriteOutcome::Rejected { ref code, .. } if code == "route_mismatch"
        ));
        Ok(())
    }

    // --- TeamsConfig::from_env ---

    #[test]
    fn runtime_config_defaults_and_positive_overrides() -> anyhow::Result<()> {
        let mut values = std::collections::HashMap::from([
            ("TEAMS_APP_ID", "app"),
            ("TEAMS_APP_SECRET", "secret"),
            ("TEAMS_DEDUPE_TTL_SECS", "42"),
            ("TEAMS_ROUTE_TTL_SECS", "84"),
            ("TEAMS_MAX_ROUTE_ENTRIES", "123"),
        ]);
        let config = TeamsConfig::from_reader(|key| values.get(key).map(ToString::to_string))
            .ok_or_else(|| anyhow::anyhow!("complete credentials should resolve"))?;
        assert_eq!(config.dedupe_ttl_secs, 42);
        assert_eq!(config.route_ttl_secs, 84);
        assert_eq!(config.max_route_entries, 123);
        assert!(!config.reactions_enabled);
        assert!(!config.inbound_attachments);

        values.insert("TEAMS_REACTIONS_ENABLED", "true");
        values.insert("TEAMS_INBOUND_ATTACHMENTS", "1");
        let config = TeamsConfig::from_reader(|key| values.get(key).map(ToString::to_string))
            .ok_or_else(|| anyhow::anyhow!("complete credentials should resolve"))?;
        assert!(config.reactions_enabled);
        assert!(config.inbound_attachments);

        values.insert("TEAMS_DEDUPE_TTL_SECS", "0");
        values.insert("TEAMS_ROUTE_TTL_SECS", "invalid");
        values.insert("TEAMS_MAX_ROUTE_ENTRIES", "0");
        values.insert("TEAMS_REACTIONS_ENABLED", "invalid");
        values.insert("TEAMS_INBOUND_ATTACHMENTS", "invalid");
        let config = TeamsConfig::from_reader(|key| values.get(key).map(ToString::to_string))
            .ok_or_else(|| anyhow::anyhow!("complete credentials should resolve"))?;
        assert_eq!(config.dedupe_ttl_secs, DEFAULT_DEDUPE_TTL_SECS);
        assert_eq!(config.route_ttl_secs, DEFAULT_ROUTE_TTL_SECS);
        assert_eq!(config.max_route_entries, DEFAULT_MAX_ROUTE_ENTRIES);
        assert!(!config.reactions_enabled);
        assert!(!config.inbound_attachments);
        Ok(())
    }

    #[test]
    fn config_from_env_returns_none_without_vars() {
        // Ensure the env vars are not set (they shouldn't be in test)
        std::env::remove_var("TEAMS_APP_ID");
        std::env::remove_var("TEAMS_APP_SECRET");
        assert!(TeamsConfig::from_env().is_none());
    }
}

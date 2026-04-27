use crate::schema::*;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// --- Bot Framework activity types ---

#[allow(dead_code)] // Bot Framework schema fields — needed for future features
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(rename = "type")]
    pub activity_type: String,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub service_url: Option<String>,
    pub channel_id: Option<String>,
    pub from: Option<ChannelAccount>,
    pub conversation: Option<ConversationAccount>,
    pub text: Option<String>,
    pub tenant: Option<TenantInfo>,
    pub channel_data: Option<ChannelData>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccount {
    pub id: Option<String>,
    pub name: Option<String>,
    pub aad_object_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAccount {
    pub id: Option<String>,
    pub conversation_type: Option<String>,
    pub is_group: Option<bool>,
    pub tenant_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantInfo {
    pub id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelData {
    pub tenant: Option<TenantInfo>,
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
}

// --- OAuth token ---

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct CachedToken {
    token: String,
    expires_at: std::time::Instant,
}

// --- Teams adapter config ---

pub struct TeamsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub oauth_endpoint: String,
    pub openid_metadata: String,
    pub allowed_tenants: Vec<String>,
}

impl TeamsConfig {
    pub fn from_env() -> Option<Self> {
        let app_id = std::env::var("TEAMS_APP_ID").ok()?;
        let app_secret = std::env::var("TEAMS_APP_SECRET").ok()?;
        Some(Self {
            app_id,
            app_secret,
            oauth_endpoint: std::env::var("TEAMS_OAUTH_ENDPOINT").unwrap_or_else(|_| {
                "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token".into()
            }),
            openid_metadata: std::env::var("TEAMS_OPENID_METADATA").unwrap_or_else(|_| {
                "https://login.botframework.com/v1/.well-known/openidconfiguration".into()
            }),
            allowed_tenants: std::env::var("TEAMS_ALLOWED_TENANTS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        })
    }
}

// --- Teams adapter state ---

pub struct TeamsAdapter {
    config: TeamsConfig,
    client: reqwest::Client,
    token_cache: RwLock<Option<CachedToken>>,
    jwks_cache: RwLock<Option<(Vec<JwkKey>, std::time::Instant)>>,
}

const JWKS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
const TOKEN_REFRESH_MARGIN: std::time::Duration = std::time::Duration::from_secs(300);

impl TeamsAdapter {
    pub fn new(config: TeamsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            token_cache: RwLock::new(None),
            jwks_cache: RwLock::new(None),
        }
    }

    /// Get a valid OAuth bearer token, refreshing if needed.
    async fn get_token(&self) -> anyhow::Result<String> {
        // Check cache
        {
            let cache = self.token_cache.read().await;
            if let Some(ref cached) = *cache {
                if cached.expires_at > std::time::Instant::now() + TOKEN_REFRESH_MARGIN {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Fetch new token
        let resp: TokenResponse = self
            .client
            .post(&self.config.oauth_endpoint)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.config.app_id),
                ("client_secret", &self.config.app_secret),
                ("scope", "https://api.botframework.com/.default"),
            ])
            .send()
            .await?
            .json()
            .await?;

        let token = resp.access_token.clone();
        *self.token_cache.write().await = Some(CachedToken {
            token: resp.access_token,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(resp.expires_in),
        });
        info!("teams OAuth token refreshed");
        Ok(token)
    }

    /// Fetch and cache JWKS signing keys from Microsoft's OpenID metadata.
    async fn get_jwks(&self) -> anyhow::Result<Vec<JwkKey>> {
        {
            let cache = self.jwks_cache.read().await;
            if let Some((ref keys, fetched_at)) = *cache {
                if fetched_at.elapsed() < JWKS_CACHE_TTL {
                    return Ok(keys.clone());
                }
            }
        }

        let config: OpenIdConfig = self
            .client
            .get(&self.config.openid_metadata)
            .send()
            .await?
            .json()
            .await?;

        let jwks: JwksResponse = self
            .client
            .get(&config.jwks_uri)
            .send()
            .await?
            .json()
            .await?;

        let keys = jwks.keys;
        *self.jwks_cache.write().await = Some((keys.clone(), std::time::Instant::now()));
        info!(count = keys.len(), "teams JWKS keys refreshed");
        Ok(keys)
    }

    /// Force-refresh JWKS keys, bypassing cache TTL. Called on cache miss (kid not found).
    async fn refresh_jwks(&self) -> anyhow::Result<Vec<JwkKey>> {
        // Invalidate cache so get_jwks fetches fresh
        *self.jwks_cache.write().await = None;
        self.get_jwks().await
    }

    /// Validate the JWT bearer token from an inbound Bot Framework request.
    pub async fn validate_jwt(&self, auth_header: &str) -> anyhow::Result<()> {
        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| anyhow::anyhow!("missing Bearer prefix"))?;

        // Decode header to get kid
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header
            .kid
            .ok_or_else(|| anyhow::anyhow!("no kid in JWT header"))?;

        let keys = self.get_jwks().await?;
        let key = match keys.iter().find(|k| k.kid.as_deref() == Some(&kid)) {
            Some(k) => k.clone(),
            None => {
                // Cache miss: Microsoft may have rotated keys. Force refresh and retry.
                let refreshed = self.refresh_jwks().await?;
                refreshed
                    .into_iter()
                    .find(|k| k.kid.as_deref() == Some(&kid))
                    .ok_or_else(|| anyhow::anyhow!("no matching JWK for kid={kid} after refresh"))?
            }
        };

        if key.kty != "RSA" {
            anyhow::bail!("unsupported key type: {}", key.kty);
        }

        let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.config.app_id]);
        // Bot Framework tokens can use RS256 or RS384
        validation.algorithms = vec![Algorithm::RS256, Algorithm::RS384];
        // Bot Framework issuer per auth spec
        validation.set_issuer(&["https://api.botframework.com"]);
        validation.validate_aud = true;
        validation.validate_exp = true;
        validation.validate_nbf = false;

        decode::<serde_json::Value>(token, &decoding_key, &validation)?;
        Ok(())
    }

    /// Check tenant allowlist.
    fn check_tenant(&self, activity: &Activity) -> bool {
        if self.config.allowed_tenants.is_empty() {
            return true;
        }
        activity
            .resolved_tenant_id()
            .is_some_and(|tid| self.config.allowed_tenants.iter().any(|a| a == tid))
    }

    /// Send a reply via Bot Framework REST API.
    pub async fn send_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
        reply_to_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_token().await?;
        let url = format!(
            "{}v3/conversations/{}/activities",
            ensure_trailing_slash(service_url),
            conversation_id
        );

        let mut body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
            "textFormat": "markdown",
        });
        if let Some(id) = reply_to_id {
            body["replyToId"] = serde_json::Value::String(id.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Bot Framework API error {status}: {body}");
        }

        let result: serde_json::Value = resp.json().await?;
        Ok(result["id"].as_str().unwrap_or("").to_string())
    }

    /// Edit an existing activity (for streaming updates).
    pub async fn update_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let url = format!(
            "{}v3/conversations/{}/activities/{}",
            ensure_trailing_slash(service_url),
            conversation_id,
            activity_id
        );

        let body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
        });

        let resp = self
            .client
            .put(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Bot Framework update error {status}: {body}");
        }
        Ok(())
    }
}

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let teams = match &state.teams {
        Some(t) => t,
        None => return StatusCode::NOT_FOUND,
    };

    // JWT validation
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Err(e) = teams.validate_jwt(auth).await {
            warn!(error = %e, "teams JWT validation failed");
            return StatusCode::UNAUTHORIZED;
        }
    } else {
        warn!("teams webhook: missing authorization header");
        return StatusCode::UNAUTHORIZED;
    }

    // Parse activity
    let activity: Activity = match serde_json::from_str(&body) {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "teams: invalid activity JSON");
            return StatusCode::BAD_REQUEST;
        }
    };

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

    let text = match activity.text.as_deref() {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => return StatusCode::OK,
    };

    let conversation_id = activity
        .conversation
        .as_ref()
        .and_then(|c| c.id.as_deref())
        .unwrap_or("");
    let conversation_type = activity
        .conversation
        .as_ref()
        .and_then(|c| c.conversation_type.as_deref())
        .unwrap_or("personal");
    let service_url = activity.service_url.as_deref().unwrap_or("");
    let sender_id = activity
        .from
        .as_ref()
        .and_then(|f| f.id.as_deref())
        .unwrap_or("");
    let sender_name = activity
        .from
        .as_ref()
        .and_then(|f| f.name.as_deref())
        .unwrap_or("Unknown");
    let activity_id = activity.id.as_deref().unwrap_or("");

    let event = GatewayEvent::new(
        "teams",
        ChannelInfo {
            id: conversation_id.to_string(),
            channel_type: conversation_type.to_string(),
            thread_id: None, // Teams conversations don't have sub-threads in the same way
        },
        SenderInfo {
            id: sender_id.to_string(),
            name: sender_name.to_string(),
            display_name: sender_name.to_string(),
            is_bot: false,
        },
        text,
        activity_id,
        vec![], // Teams @mentions parsing deferred to future PR
    );

    // Store service_url for reply routing
    state.teams_service_urls.lock().await.insert(
        conversation_id.to_string(),
        (service_url.to_string(), std::time::Instant::now()),
    );

    let json = serde_json::to_string(&event).unwrap();
    let tenant_id = activity.resolved_tenant_id().unwrap_or("");
    info!(
        conversation = conversation_id,
        sender = sender_name,
        tenant = tenant_id,
        service_url = service_url,
        "teams → gateway"
    );
    let _ = state.event_tx.send(json);

    StatusCode::OK
}

// --- Reply handler ---

pub async fn handle_reply(
    reply: &GatewayReply,
    teams: &TeamsAdapter,
    service_urls: &tokio::sync::Mutex<
        std::collections::HashMap<String, (String, std::time::Instant)>,
    >,
) {
    // Reactions are not supported on Teams — silently ignore
    if reply.command.as_deref() == Some("add_reaction")
        || reply.command.as_deref() == Some("remove_reaction")
    {
        return;
    }

    let service_url = {
        let mut urls = service_urls.lock().await;
        match urls.get_mut(&reply.channel.id) {
            Some((url, ts)) => {
                // Refresh timestamp on reply to prevent TTL expiry during active conversations
                *ts = std::time::Instant::now();
                url.clone()
            }
            None => {
                error!(conversation = %reply.channel.id, "teams: no service_url for conversation");
                return;
            }
        }
    };

    let reply_to_id = if reply.reply_to.is_empty() {
        None
    } else {
        Some(reply.reply_to.as_str())
    };

    info!(conversation = %reply.channel.id, "gateway → teams");
    match teams
        .send_activity(
            &service_url,
            &reply.channel.id,
            &reply.content.text,
            reply_to_id,
        )
        .await
    {
        Ok(id) => debug!(activity_id = %id, "teams activity sent"),
        Err(e) => error!(error = %e, "teams send error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ensure_trailing_slash ---

    #[test]
    fn trailing_slash_adds_when_missing() {
        assert_eq!(
            ensure_trailing_slash("https://example.com"),
            "https://example.com/"
        );
    }

    #[test]
    fn trailing_slash_keeps_when_present() {
        assert_eq!(
            ensure_trailing_slash("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn trailing_slash_empty_string() {
        assert_eq!(ensure_trailing_slash(""), "/");
    }

    // --- check_tenant ---

    fn make_config(tenants: Vec<&str>) -> TeamsConfig {
        TeamsConfig {
            app_id: "test-app".into(),
            app_secret: "test-secret".into(),
            oauth_endpoint: "https://example.com/token".into(),
            openid_metadata: "https://example.com/openid".into(),
            allowed_tenants: tenants.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_activity_with_tenant(tenant_id: Option<&str>) -> Activity {
        Activity {
            activity_type: "message".into(),
            id: Some("act1".into()),
            timestamp: None,
            service_url: Some("https://smba.trafficmanager.net/".into()),
            channel_id: Some("msteams".into()),
            from: None,
            conversation: None,
            text: Some("hello".into()),
            tenant: tenant_id.map(|id| TenantInfo {
                id: Some(id.into()),
            }),
            channel_data: None,
        }
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
    fn resolved_tenant_falls_back_to_channel_data() {
        // Teams personal/channel webhooks put tenant in channelData, not top-level
        let json = r#"{
            "type": "message",
            "channelData": {"tenant": {"id": "from-channel-data"}}
        }"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.resolved_tenant_id(), Some("from-channel-data"));
    }

    #[test]
    fn resolved_tenant_prefers_top_level_over_channel_data() {
        let json = r#"{
            "type": "message",
            "tenant": {"id": "top-level"},
            "channelData": {"tenant": {"id": "from-channel-data"}}
        }"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.resolved_tenant_id(), Some("top-level"));
    }

    #[test]
    fn resolved_tenant_falls_back_to_conversation_tenant_id() {
        let json = r#"{
            "type": "message",
            "conversation": {"id": "c1", "tenantId": "from-conversation"}
        }"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.resolved_tenant_id(), Some("from-conversation"));
    }

    #[test]
    fn resolved_tenant_returns_none_when_absent() {
        let json = r#"{"type": "message"}"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.resolved_tenant_id(), None);
    }

    // --- validate_jwt error paths ---

    #[tokio::test]
    async fn jwt_rejects_missing_bearer_prefix() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let result = adapter.validate_jwt("NotBearer xyz").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Bearer"));
    }

    #[tokio::test]
    async fn jwt_rejects_empty_bearer() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let result = adapter.validate_jwt("Bearer ").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn jwt_rejects_garbage_token() {
        let adapter = TeamsAdapter::new(make_config(vec![]));
        let result = adapter.validate_jwt("Bearer not.a.valid.jwt").await;
        assert!(result.is_err());
    }

    // --- Activity deserialization ---

    #[test]
    fn deserialize_minimal_activity() {
        let json = r#"{"type": "message"}"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "message");
        assert!(activity.text.is_none());
        assert!(activity.from.is_none());
    }

    #[test]
    fn deserialize_full_activity() {
        let json = r#"{
            "type": "message",
            "id": "act123",
            "serviceUrl": "https://smba.trafficmanager.net/",
            "channelId": "msteams",
            "from": {"id": "user1", "name": "Alice", "aadObjectId": "aad-123"},
            "conversation": {"id": "conv1", "conversationType": "personal", "isGroup": false},
            "text": "hello bot",
            "tenant": {"id": "tenant-abc"}
        }"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "message");
        assert_eq!(activity.text.as_deref(), Some("hello bot"));
        assert_eq!(
            activity.from.as_ref().unwrap().name.as_deref(),
            Some("Alice")
        );
        assert_eq!(
            activity.tenant.as_ref().unwrap().id.as_deref(),
            Some("tenant-abc")
        );
    }

    #[test]
    fn deserialize_non_message_activity() {
        let json = r#"{"type": "conversationUpdate"}"#;
        let activity: Activity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "conversationUpdate");
    }

    #[test]
    fn deserialize_invalid_json_fails() {
        let result = serde_json::from_str::<Activity>("not json");
        assert!(result.is_err());
    }

    // --- TeamsConfig::from_env ---

    #[test]
    fn config_from_env_returns_none_without_vars() {
        // Ensure the env vars are not set (they shouldn't be in test)
        std::env::remove_var("TEAMS_APP_ID");
        std::env::remove_var("TEAMS_APP_SECRET");
        assert!(TeamsConfig::from_env().is_none());
    }
}

use crate::schema::*;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
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
        })
    }
}

// --- Teams adapter state ---

pub struct TeamsAdapter {
    config: TeamsConfig,
    client: reqwest::Client,
    token_cache: RwLock<Option<CachedToken>>,
    token_refresh_lock: Mutex<()>,
    openid_cache: RwLock<Option<CachedOpenId>>,
    openid_refresh_lock: Mutex<()>,
    jwks_cache: RwLock<Option<CachedJwks>>,
    jwks_refresh_lock: Mutex<()>,
    allow_non_public_endpoints: bool,
}

const AUTH_CACHE_TTL: Duration = Duration::from_secs(3600);
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);
const TEAMS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TEAMS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEAMS_ERROR_BODY_LIMIT: usize = 4 * 1024;
const TEAMS_MAX_REDIRECTS: usize = 5;
const TEAMS_PUBLIC_SERVICE_HOST: &str = "smba.trafficmanager.net";
const TEAMS_PUBLIC_OAUTH_HOST: &str = "login.microsoftonline.com";
const TEAMS_PUBLIC_OPENID_HOST: &str = "login.botframework.com";

impl TeamsAdapter {
    pub fn new(config: TeamsConfig) -> Self {
        Self::with_client(config, build_http_client(TEAMS_REQUEST_TIMEOUT), false)
    }

    fn with_client(
        config: TeamsConfig,
        client: reqwest::Client,
        allow_non_public_endpoints: bool,
    ) -> Self {
        Self {
            config,
            client,
            token_cache: RwLock::new(None),
            token_refresh_lock: Mutex::new(()),
            openid_cache: RwLock::new(None),
            openid_refresh_lock: Mutex::new(()),
            jwks_cache: RwLock::new(None),
            jwks_refresh_lock: Mutex::new(()),
            allow_non_public_endpoints,
        }
    }

    #[cfg(test)]
    fn new_for_test(config: TeamsConfig) -> Self {
        Self::with_client(config, build_http_client(TEAMS_REQUEST_TIMEOUT), true)
    }

    #[cfg(test)]
    fn new_for_test_with_timeout(config: TeamsConfig, request_timeout: Duration) -> Self {
        Self::with_client(config, build_http_client(request_timeout), true)
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

    fn validate_service_url(&self, service_url: &str) -> anyhow::Result<reqwest::Url> {
        validate_public_cloud_endpoint(
            service_url,
            "Teams service URL",
            TEAMS_PUBLIC_SERVICE_HOST,
            self.allow_non_public_endpoints,
        )
    }

    /// Send a reply via Bot Framework REST API.
    pub async fn send_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
        reply_to_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = self.connector_url(service_url, conversation_id, None)?;
        let token = self.get_token().await?;

        let mut body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
            "textFormat": "markdown",
        });
        if let Some(id) = reply_to_id {
            body["replyToId"] = serde_json::Value::String(id.to_string());
        }

        let response = self
            .client
            .post(url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|error| safe_request_error("Bot Framework send", &error))?;
        let response =
            require_http_success(response, "Bot Framework send", &[token.as_str()]).await?;
        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Bot Framework send response was not valid JSON"))?;
        result
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Bot Framework send response missing activity id"))
    }

    /// Edit an existing activity (for streaming updates).
    pub async fn update_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let url = self.connector_url(service_url, conversation_id, Some(activity_id))?;
        let token = self.get_token().await?;
        let body = serde_json::json!({
            "type": "message",
            "from": { "id": &self.config.app_id },
            "text": text,
            "textFormat": "markdown",
        });

        let response = self
            .client
            .put(url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|error| safe_request_error("Bot Framework update", &error))?;
        require_http_success(response, "Bot Framework update", &[token.as_str()]).await?;
        Ok(())
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

    // B3: Guard against an absent or unsafe service URL before persisting a
    // reply route. JWT validation binds this value to Microsoft; the public-
    // cloud policy additionally prevents credential-bearing SSRF.
    if service_url.is_empty() {
        warn!("teams: activity missing service_url, cannot route replies");
        return StatusCode::OK;
    }
    let validated_service_url = match teams.validate_service_url(service_url) {
        Ok(url) => url,
        Err(error) => {
            warn!(reason = %error, "teams: activity has unsafe service_url");
            return StatusCode::BAD_REQUEST;
        }
    };

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
        (validated_service_url.to_string(), Instant::now()),
    );

    let json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(serialization_error) => {
            error!(error = %serialization_error, "teams: failed to serialize gateway event");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    let tenant_id = activity.resolved_tenant_id().unwrap_or("");
    info!(
        conversation = conversation_id,
        sender = sender_name,
        tenant = tenant_id,
        service_host = validated_service_url.host_str().unwrap_or("unknown"),
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
) -> anyhow::Result<Option<String>> {
    // Fail closed for commands the Teams adapter does not implement. Falling
    // through to `send_activity` would turn edit/delete/topic commands into new
    // messages, producing duplicate or misleading output. Reaction commands
    // remain an intentional no-op until a Teams status backend is implemented.
    match reply.command.as_deref() {
        None => {}
        Some("add_reaction" | "remove_reaction") => {
            debug!(command = ?reply.command.as_deref(), "teams: ignoring unsupported reaction command");
            return Ok(None);
        }
        Some(command) => anyhow::bail!("unsupported Teams command: {command}"),
    }

    let service_url = {
        let mut urls = service_urls.lock().await;
        match urls.get_mut(&reply.channel.id) {
            Some((url, ts)) => {
                // Refresh timestamp on reply to prevent TTL expiry during active conversations
                *ts = std::time::Instant::now();
                url.clone()
            }
            None => anyhow::bail!(
                "no Teams service_url for conversation {}",
                reply.channel.id
            ),
        }
    };

    let reply_to_id = if reply.reply_to.is_empty() {
        None
    } else {
        Some(reply.reply_to.as_str())
    };

    info!(conversation = %reply.channel.id, "gateway → teams");
    let id = teams
        .send_activity(
            &service_url,
            &reply.channel.id,
            &reply.content.text,
            reply_to_id,
        )
        .await?;
    debug!(activity_id = %id, "teams activity sent");
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    // --- Bot Connector URL and error hardening ---

    #[test]
    fn connector_url_encodes_segments_and_preserves_service_path() {
        let url = connector_url(
            "https://smba.trafficmanager.net/teams/",
            "a/b?c",
            Some("message id/%"),
            false,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://smba.trafficmanager.net/teams/v3/conversations/a%2Fb%3Fc/activities/message%20id%2F%25"
        );
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

    fn make_reply(command: Option<&str>) -> GatewayReply {
        GatewayReply {
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
        let result = adapter.validate_jwt("Bearer not.a.valid.jwt", &activity).await;
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

    // --- transport concurrency and HTTP policy ---

    #[tokio::test]
    async fn concurrent_oauth_callers_share_one_refresh() {
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
        assert_eq!(first.unwrap(), "singleflight-token");
        assert_eq!(second.unwrap(), "singleflight-token");
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
    async fn concurrent_jwks_callers_share_metadata_and_key_fetches() {
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
        assert_eq!(first.unwrap().keys.len(), 1);
        assert_eq!(second.unwrap().keys.len(), 1);
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
    async fn connector_success_without_activity_id_is_rejected() {
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

        let error = adapter
            .send_activity(&server.uri(), "conversation-1", "hello", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing activity id"));
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

        let error = adapter
            .send_activity(&server.uri(), "conversation-1", "hello", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("500"));
        assert!(error.contains("[truncated]"));
        assert!(!error.contains("leaked-token"));
        assert!(!error.contains("test-token"));
        assert!(!error.contains("sensitive.example"));
        assert!(error.len() <= TEAMS_ERROR_BODY_LIMIT + 256);
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

        let error = adapter
            .send_activity(&server.uri(), "conversation-1", "hello", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("timed out"));
        assert!(!error.contains(&server.uri()));
    }

    // --- reply command dispatch ---

    #[tokio::test]
    async fn unsupported_commands_never_fall_through_to_send_activity() {
        let server = MockServer::start().await;
        let _no_post = Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_config(vec![]);
        config.oauth_endpoint = format!("{}/token", server.uri());
        let adapter = TeamsAdapter::new_for_test(config);
        let service_urls = tokio::sync::Mutex::new(std::collections::HashMap::from([(
            "conversation-1".to_string(),
            (server.uri(), std::time::Instant::now()),
        )]));

        for command in ["add_reaction", "remove_reaction"] {
            let outcome = handle_reply(&make_reply(Some(command)), &adapter, &service_urls)
                .await
                .unwrap();
            assert_eq!(outcome, None, "reaction command {command} should be a no-op");
        }

        for command in [
            "create_topic",
            "edit_message",
            "delete_message",
            "future_unknown_command",
        ] {
            let error = handle_reply(&make_reply(Some(command)), &adapter, &service_urls)
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains(command),
                "error should identify unsupported command {command}"
            );
        }
    }

    #[tokio::test]
    async fn commandless_reply_still_sends_one_activity() {
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "activity-1"})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = make_config(vec![]);
        config.oauth_endpoint = format!("{}/token", server.uri());
        let adapter = TeamsAdapter::new_for_test(config);
        let service_urls = tokio::sync::Mutex::new(std::collections::HashMap::from([(
            "conversation-1".to_string(),
            (server.uri(), std::time::Instant::now()),
        )]));

        let message_id = handle_reply(&make_reply(None), &adapter, &service_urls)
            .await
            .unwrap();
        assert_eq!(message_id.as_deref(), Some("activity-1"));
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

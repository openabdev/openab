//! LINE WORKS bot adapter.
//!
//! Webhook-only platform: LINE WORKS POSTs one event object per request to the
//! registered callback URL, signed with the Bot Secret. Outbound messages go
//! through the REST API (`https://www.worksapis.com/v1.0`) authenticated with
//! an OAuth 2.0 service-account JWT flow (same jwt-bearer grant as Google
//! Chat, see `googlechat::GoogleChatTokenCache`).
//!
//! Platform limits that shape this adapter: no message edit/delete (so no
//! streaming), no reactions, no threads, plain-text only, 10,000-char text
//! limit, and no reply-token mechanism (push-style sends only).

use crate::media::{
    audio_extension, format_bytes, is_text_extension, resize_and_compress, AUDIO_MAX_DOWNLOAD,
    FILE_MAX_DOWNLOAD, IMAGE_MAX_DOWNLOAD,
};
use crate::schema::*;
use crate::store;
use axum::extract::State;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Base URL for the LINE WORKS REST API. Overridden in tests.
pub const LINEWORKS_API_BASE: &str = "https://www.worksapis.com/v1.0";
/// Token endpoint for the service-account JWT exchange. Overridden in tests.
pub const LINEWORKS_AUTH_BASE: &str = "https://auth.worksmobile.com";

/// Maximum length of one text message. Longer replies are split.
pub const LINEWORKS_TEXT_LIMIT: usize = 10_000;

/// Channel-id prefix marking a 1:1 conversation (LINE WORKS has separate
/// send endpoints for users and channels; `GatewayReply` only carries an
/// opaque channel id, so the distinction is encoded in the id itself).
const USER_CHANNEL_PREFIX: &str = "user:";

// --- Config ---

pub struct LineWorksConfig {
    pub bot_id: String,
    pub bot_secret: String,
    pub client_id: String,
    pub client_secret: String,
    pub service_account: String,
    pub private_key: String,
    pub webhook_path: String,
    pub api_base: String,
    pub auth_base: String,
    /// Channel (group) messages must @-mention the bot to be forwarded.
    /// 1:1 messages always pass. Default: true. Set false for ambient mode.
    pub require_mention: bool,
    /// Bot display name used for mention matching. When unset, fetched once
    /// from `GET /bots/{botId}` and cached.
    pub bot_name: Option<String>,
    /// Render markdown replies as flexible-template (flex) messages.
    /// Falls back to plain text when the reply has no markdown, exceeds the
    /// flex size limits, or the API rejects the payload. Default: true.
    pub rich_messages: bool,
    /// Short acknowledgement message sent immediately when a user message is
    /// accepted for processing. LINE WORKS has no reaction or typing
    /// indicator API, so without this the user sees nothing until the full
    /// reply lands. Unset/empty = disabled.
    pub ack_message: Option<String>,
}

/// Parse-check RS256 private-key PEM material. Activation and construction
/// share this so an invalid key fails fast at startup instead of on the
/// first token exchange.
pub fn valid_private_key(pem: &str) -> bool {
    jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).is_ok()
}

impl LineWorksConfig {
    /// Build from `LINEWORKS_*` env vars. Returns `None` (adapter disabled)
    /// unless bot id, bot secret, and the full auth material are all present.
    pub fn from_env() -> Option<Self> {
        Self::from_reader(|k| std::env::var(k).ok())
    }

    /// Build from any `LINEWORKS_*` key reader. The `[lineworks]` config
    /// bridge (`apply_lineworks_config`) goes through here too, so config-
    /// and env-derived adapters share the same validation.
    pub fn from_reader(read: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let read_nonempty = |k: &str| read(k).filter(|v| !v.is_empty());
        let bot_id = read_nonempty("LINEWORKS_BOT_ID")?;
        let bot_secret = read_nonempty("LINEWORKS_BOT_SECRET")?;
        let client_id = read_nonempty("LINEWORKS_CLIENT_ID")?;
        let client_secret = read_nonempty("LINEWORKS_CLIENT_SECRET")?;
        let service_account = read_nonempty("LINEWORKS_SERVICE_ACCOUNT")?;
        let private_key = match read_nonempty("LINEWORKS_PRIVATE_KEY") {
            Some(pem) => pem,
            None => {
                let path = read_nonempty("LINEWORKS_PRIVATE_KEY_FILE")?;
                match std::fs::read_to_string(&path) {
                    Ok(pem) => pem,
                    Err(e) => {
                        error!(path = %path, err = %e, "lineworks: cannot read private key file");
                        return None;
                    }
                }
            }
        };
        if !valid_private_key(&private_key) {
            error!("lineworks: private key is not valid RS256 PEM material — adapter disabled");
            return None;
        }
        Some(Self {
            bot_id,
            bot_secret,
            client_id,
            client_secret,
            service_account,
            private_key,
            webhook_path: read_nonempty("LINEWORKS_WEBHOOK_PATH")
                .unwrap_or_else(|| "/webhook/lineworks".into()),
            api_base: LINEWORKS_API_BASE.into(),
            auth_base: LINEWORKS_AUTH_BASE.into(),
            require_mention: read_nonempty("LINEWORKS_REQUIRE_MENTION")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true),
            bot_name: read_nonempty("LINEWORKS_BOT_NAME"),
            rich_messages: read_nonempty("LINEWORKS_RICH_MESSAGES")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true),
            ack_message: read_nonempty("LINEWORKS_ACK_MESSAGE"),
        })
    }
}

/// Retry cooldown after a failed bot-name lookup.
const BOT_NAME_RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Default)]
struct BotNameCache {
    name: Option<String>,
    last_failure: Option<Instant>,
}

pub struct LineWorksAdapter {
    pub config: LineWorksConfig,
    token_cache: LineWorksTokenCache,
    /// Bot display name for mention matching, fetched lazily from the API
    /// when `config.bot_name` is unset. A `Mutex` (not `RwLock`) held across
    /// the fetch gives single-flight semantics: concurrent first-use events
    /// wait for one lookup instead of duplicating token + bot-info requests.
    /// A failed lookup records `last_failure` so an upstream outage is
    /// retried at most once per cooldown window instead of on every event.
    bot_name_cache: tokio::sync::Mutex<BotNameCache>,
    /// Shared client for attachment downloads: redirects disabled so the
    /// Authorization header can be re-attached manually on the cross-host
    /// hop. One client per adapter keeps connection pooling on the hot path.
    download_client: reqwest::Client,
}

impl LineWorksAdapter {
    pub fn new(config: LineWorksConfig) -> Self {
        let download_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("lineworks download client must build");
        Self {
            token_cache: LineWorksTokenCache::new(),
            bot_name_cache: tokio::sync::Mutex::new(BotNameCache::default()),
            download_client,
            config,
        }
    }

    /// Resolve the bot display name: config override → cached → fetch from
    /// `GET /bots/{botId}`. Returns `None` when the name cannot be determined
    /// (callers fail open so a Console/API hiccup never bricks the bot).
    async fn bot_name(&self, client: &reqwest::Client) -> Option<String> {
        if let Some(ref name) = self.config.bot_name {
            return Some(name.clone());
        }
        // Single-flight: the lock is held across the network fetch so
        // concurrent callers wait for the first result instead of racing.
        let mut cache = self.bot_name_cache.lock().await;
        if let Some(ref name) = cache.name {
            return Some(name.clone());
        }
        // Negative cache: during an upstream outage, retry at most once per
        // cooldown window; other events return fast (gate fails open, which
        // is the documented tradeoff — cheaply, instead of per-event I/O).
        if let Some(failed_at) = cache.last_failure {
            if failed_at.elapsed() < BOT_NAME_RETRY_COOLDOWN {
                return None;
            }
        }
        let fail = |cache: &mut BotNameCache| {
            cache.last_failure = Some(Instant::now());
        };
        let token = match self.token_cache.get_token(client, &self.config).await {
            Ok(t) => t,
            Err(e) => {
                warn!(err = %e, "lineworks: cannot get token for bot-name lookup");
                fail(&mut cache);
                return None;
            }
        };
        let url = format!("{}/bots/{}", self.config.api_base, self.config.bot_id);
        let body: serde_json::Value = match client.get(&url).bearer_auth(&token).send().await {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(b) => b,
                Err(e) => {
                    warn!(err = %e, "lineworks: bot info parse failed");
                    fail(&mut cache);
                    return None;
                }
            },
            Ok(r) => {
                warn!(status = %r.status(), "lineworks: bot info request failed");
                fail(&mut cache);
                return None;
            }
            Err(e) => {
                warn!(err = %e, "lineworks: bot info request error");
                fail(&mut cache);
                return None;
            }
        };
        let Some(name) = body
            .get("botName")
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            fail(&mut cache);
            return None;
        };
        info!(bot_name = %name, "lineworks: resolved bot name for mention gating");
        cache.name = Some(name.clone());
        cache.last_failure = None;
        Some(name)
    }
}

/// Mention gate for channel (group) messages. Returns `true` when the event
/// should be forwarded; on a hit, strips the `@BotName` mention from the text.
/// Fail-open: if the bot name cannot be resolved, the event passes.
async fn passes_mention_gate(
    adapter: &LineWorksAdapter,
    client: &reqwest::Client,
    event: &mut GatewayEvent,
) -> bool {
    if !adapter.config.require_mention || event.channel.channel_type != "channel" {
        return true;
    }
    // Attachment-only events have no text to match; treat like unmentioned.
    let Some(name) = adapter.bot_name(client).await else {
        warn!("lineworks: bot name unavailable, mention gate fails open");
        return true;
    };
    let mention = format!("@{name}");
    match strip_boundary_mention(&event.content.text, &mention) {
        Some(stripped) => {
            event.content.text = stripped;
            true
        }
        None => {
            info!(
                channel = %event.channel.id,
                "lineworks channel message dropped (mention gating: bot not mentioned)"
            );
            false
        }
    }
}

/// Boundary-aware mention matching: `@Bot` must not match inside `@Bottage`.
/// A match requires the character after the mention to be absent or
/// non-alphanumeric. Returns the text with every boundary-valid mention
/// removed (trimmed), or `None` when no valid mention exists.
fn strip_boundary_mention(text: &str, mention: &str) -> Option<String> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    let mut matched = false;
    while let Some(pos) = rest.find(mention) {
        let after = rest[pos + mention.len()..].chars().next();
        if after.is_none_or(|c| !c.is_alphanumeric()) {
            matched = true;
            result.push_str(&rest[..pos]);
        } else {
            // Not a boundary match ("@Bottage") — keep the text as-is.
            result.push_str(&rest[..pos + mention.len()]);
        }
        rest = &rest[pos + mention.len()..];
    }
    result.push_str(rest);
    matched.then(|| result.trim().to_string())
}

// --- Token cache with JWT auto-refresh ---

const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

struct LineWorksTokenCache {
    token: RwLock<Option<(String, Instant, u64)>>,
}

impl LineWorksTokenCache {
    fn new() -> Self {
        Self {
            token: RwLock::new(None),
        }
    }

    async fn get_token(
        &self,
        client: &reqwest::Client,
        config: &LineWorksConfig,
    ) -> Result<String, String> {
        {
            let guard = self.token.read().await;
            if let Some((ref tok, ref ts, ttl)) = *guard {
                if ts.elapsed().as_secs() < ttl.saturating_sub(TOKEN_REFRESH_MARGIN_SECS) {
                    return Ok(tok.clone());
                }
            }
        }
        let mut guard = self.token.write().await;
        if let Some((ref tok, ref ts, ttl)) = *guard {
            if ts.elapsed().as_secs() < ttl.saturating_sub(TOKEN_REFRESH_MARGIN_SECS) {
                return Ok(tok.clone());
            }
        }
        let (new_token, expire) = refresh_token(client, config).await?;
        *guard = Some((new_token.clone(), Instant::now(), expire));
        info!("lineworks access token refreshed (expires in {expire}s)");
        Ok(new_token)
    }

    /// Drop the cached token only if it is still the one that failed, so a
    /// delayed 401 from an old token cannot clear a token another concurrent
    /// task just refreshed (which would force an avoidable serialized token
    /// exchange). Used when the API answers 401 despite a locally-unexpired
    /// token (e.g. revoked in the Developer Console).
    async fn invalidate_if(&self, failed_token: &str) {
        let mut guard = self.token.write().await;
        if matches!(*guard, Some((ref tok, _, _)) if tok == failed_token) {
            *guard = None;
        }
    }
}

async fn refresh_token(
    client: &reqwest::Client,
    config: &LineWorksConfig,
) -> Result<(String, u64), String> {
    let jwt = build_jwt(config).map_err(|e| format!("JWT build error: {e}"))?;
    let resp = client
        .post(format!("{}/oauth2/v2.0/token", config.auth_base))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
            ("client_id", &config.client_id),
            ("client_secret", &config.client_secret),
            // Least-privilege: message send + read-only bot details (the
            // bot-name lookup for mention gating). Deliberately NOT the
            // broad read/write `bot` scope.
            ("scope", "bot.message,bot.read"),
        ])
        .send()
        .await
        .map_err(|e| format!("token exchange request failed: {e}"))?;

    // Surface upstream HTTP failures as such — parsing a 4xx/5xx body as
    // JSON first would turn them into misleading parse errors.
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        return Err(format!(
            "token endpoint HTTP {}: {snippet}",
            status.as_u16()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("token exchange parse failed: {e}"))?;

    let token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let err = body
                .get("error_description")
                .and_then(|v| v.as_str())
                .or_else(|| body.get("error").and_then(|v| v.as_str()))
                .unwrap_or("unknown error");
            format!("token exchange failed: {err}")
        })?
        .to_string();

    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        // Some responses carry expires_in as a JSON string.
        .or_else(|| {
            body.get("expires_in")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(3600);

    Ok((token, expires_in))
}

fn build_jwt(config: &LineWorksConfig) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let claims = serde_json::json!({
        "iss": config.client_id,
        "sub": config.service_account,
        "iat": now,
        "exp": now + 3600,
    });

    let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())
        .map_err(|e| format!("RSA key parse error: {e}"))?;
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    jsonwebtoken::encode(&header, &claims, &key).map_err(|e| format!("JWT encode error: {e}"))
}

// --- Webhook types (one event object per request) ---

#[derive(Debug, Deserialize)]
pub struct LineWorksEvent {
    #[serde(rename = "type")]
    event_type: String,
    source: Option<LineWorksSource>,
    content: Option<LineWorksContent>,
}

#[derive(Debug, Deserialize)]
struct LineWorksSource {
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "channelId")]
    channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineWorksContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
    #[serde(rename = "fileId")]
    file_id: Option<String>,
    #[serde(rename = "fileName")]
    file_name: Option<String>,
}

// --- Webhook handler ---

/// Verify `X-WORKS-Signature`: Base64(HMAC-SHA256(raw body, Bot Secret)).
fn verify_signature(bot_secret: &str, body: &[u8], signature: &str) -> bool {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use subtle::ConstantTimeEq;

    let mut mac = Hmac::<Sha256>::new_from_slice(bot_secret.as_bytes()).expect("HMAC key");
    mac.update(body);
    let expected = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    expected.as_bytes().ct_eq(signature.as_bytes()).into()
}

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    let Some(ref adapter) = state.lineworks else {
        warn!("lineworks webhook hit but adapter not configured");
        return axum::http::StatusCode::SERVICE_UNAVAILABLE;
    };

    let signature = headers
        .get("x-works-signature")
        .and_then(|v| v.to_str().ok());
    let Some(signature) = signature else {
        warn!("lineworks webhook rejected: missing X-WORKS-Signature");
        return axum::http::StatusCode::UNAUTHORIZED;
    };
    if !verify_signature(&adapter.config.bot_secret, &body, signature) {
        warn!("lineworks webhook rejected: invalid signature");
        return axum::http::StatusCode::UNAUTHORIZED;
    }

    // The platform contract marks X-WORKS-BotId as a required header — a
    // signed request without it is malformed/misrouted and is rejected.
    let bot_id_header = headers.get("x-works-botid").and_then(|v| v.to_str().ok());
    match bot_id_header {
        None => {
            warn!("lineworks webhook rejected: missing X-WORKS-BotId");
            return axum::http::StatusCode::UNAUTHORIZED;
        }
        Some(bot_id) if bot_id != adapter.config.bot_id => {
            warn!(
                got = %bot_id,
                expected = %adapter.config.bot_id,
                "lineworks webhook rejected: bot id mismatch"
            );
            return axum::http::StatusCode::UNAUTHORIZED;
        }
        Some(_) => {}
    }

    let event: LineWorksEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("lineworks webhook parse error: {e}");
            return axum::http::StatusCode::BAD_REQUEST;
        }
    };

    // Cheap, pure classification only — attachment I/O happens post-ack.
    let Some((gateway_event, pending)) = classify_event(&event) else {
        return axum::http::StatusCode::OK;
    };

    // An acknowledged callback must correspond to an acceptable event: the
    // platform does not resend failed callbacks, so if no consumer is
    // attached the event would be silently lost. Refuse the ack instead.
    if state.event_tx.receiver_count() == 0 {
        error!("lineworks webhook refused: no gateway event consumer attached");
        return axum::http::StatusCode::SERVICE_UNAVAILABLE;
    }

    // Bounded two-tier ingress: the worker pool bounds concurrent slow work
    // (ack message + attachment download), while a larger queue absorbs
    // bursts so a signed, valid callback is never rejected merely because
    // every worker is busy — LINE WORKS does not resend callbacks. Only
    // queue overflow (LINEWORKS_INGRESS_QUEUE_MAX callbacks already waiting)
    // answers 503, and that drop is loud (error log) and bounded by design.
    let queue_slot = match state.lineworks_ingress_queue.clone().try_acquire_owned() {
        Ok(slot) => slot,
        Err(tokio::sync::TryAcquireError::NoPermits) => {
            error!(
                event_id = %gateway_event.event_id,
                "lineworks webhook refused: ingress queue overflow (event dropped by full policy)"
            );
            return axum::http::StatusCode::SERVICE_UNAVAILABLE;
        }
        Err(tokio::sync::TryAcquireError::Closed) => {
            warn!("lineworks webhook ingress queue closed unexpectedly");
            return axum::http::StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    let adapter = adapter.clone();
    let background_state = state.clone();
    tokio::spawn(async move {
        // Wait for a worker permit (bounded by the held queue slot), then
        // release the queue slot so the next callback can be accepted.
        let permit = match background_state
            .lineworks_webhook_semaphore
            .clone()
            .acquire_owned()
            .await
        {
            Ok(permit) => permit,
            Err(_) => {
                error!(
                    event_id = %gateway_event.event_id,
                    "lineworks worker semaphore closed; queued event dropped"
                );
                return;
            }
        };
        drop(queue_slot);
        let _permit = permit;
        process_lineworks_event(background_state, adapter, gateway_event, pending).await;
    });

    axum::http::StatusCode::OK
}

/// Post-ack worker: authorize (mention gate) → download attachments →
/// broadcast. Runs under the bounded webhook semaphore.
///
/// Loss policy: once the callback is acknowledged, a failure here (download
/// error, closed broadcast channel) drops the event — LINE WORKS does not
/// resend callbacks. Failures are logged at error level; the pre-ack
/// receiver check keeps the "acked but nobody listening" window to
/// consumer restarts only.
async fn process_lineworks_event(
    state: Arc<crate::AppState>,
    adapter: Arc<LineWorksAdapter>,
    mut gateway_event: GatewayEvent,
    pending: Option<PendingAttachment>,
) {
    // Authorization before any outbound work: a dropped event must not
    // consume downloads, tokens, storage — or an ack message.
    if !passes_mention_gate(&adapter, &state.client, &mut gateway_event).await {
        return;
    }
    if pending.is_none() && gateway_event.content.text.trim().is_empty() {
        // Mention stripping can leave an empty prompt ("@Bot" alone).
        return;
    }

    // Identity trust probe for ALL accepted events (not just attachments):
    // untrusted senders get no ack and no download; the event still
    // broadcasts so the core ingress gate can deny + echo request-access.
    let sender_trusted = state
        .trust_probe
        .as_ref()
        .map(|probe| {
            probe(
                "lineworks",
                &gateway_event.channel.id,
                &gateway_event.sender.id,
            )
        })
        .unwrap_or(true);

    // Receipt ack: LINE WORKS has no reactions/typing indicator, so a short
    // message is the only "working on it" signal. Awaited inline INSIDE the
    // bounded worker (after the gates, before the download) so a burst can
    // never fan out unbounded outbound tasks or ack a denied sender.
    if sender_trusted {
        if let Some(ref ack) = adapter.config.ack_message {
            let url = send_endpoint(
                &adapter.config.api_base,
                &adapter.config.bot_id,
                &gateway_event.channel.id,
            );
            let body = serde_json::json!({"content": {"type": "text", "text": ack}});
            if !send_body(&state.client, &adapter, &url, &body).await {
                warn!("lineworks: ack message send failed");
            }
        }
    }

    if let Some(p) = pending {
        if sender_trusted {
            gateway_event.content.attachments.push(
                download_attachment(&adapter, &p.kind, &p.file_id, p.file_name.as_deref()).await,
            );
        } else {
            info!(
                sender = %gateway_event.sender.id,
                "lineworks: sender not trusted — attachment download skipped"
            );
            gateway_event.content.attachments.push(Attachment::rejected(
                if p.kind == "image" {
                    "image"
                } else {
                    "text_file"
                },
                format!("lineworks_{}", p.file_id),
                "application/octet-stream",
                0,
                "security rejected: sender not authorized; attachment not downloaded",
            ));
        }
    }

    let json = serde_json::to_string(&gateway_event).unwrap();
    info!(
        channel = %gateway_event.channel.id,
        sender = %gateway_event.sender.id,
        "lineworks → gateway"
    );
    if let Err(e) = state.event_tx.send(json) {
        // The pre-ack receiver check makes this a narrow race (consumer went
        // away mid-processing); surfaced loudly because the event is lost.
        error!(err = %e, event_id = %gateway_event.event_id, "lineworks: event enqueue failed after ack — event lost");
    }
}

/// Fetch attachment bytes from the LINE WORKS content-download API.
///
/// `GET /bots/{botId}/attachments/{fileId}` answers 302 with a storage URL
/// that itself requires the Authorization header, and reqwest strips auth on
/// cross-host redirects — so redirects are followed manually (bounded).
async fn fetch_attachment_bytes(
    adapter: &LineWorksAdapter,
    file_id: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, String), String> {
    let client = &adapter.download_client;
    let token = adapter
        .token_cache
        .get_token(client, &adapter.config)
        .await
        .map_err(|e| format!("token error: {e}"))?;

    let mut url = format!(
        "{}/bots/{}/attachments/{}",
        adapter.config.api_base, adapter.config.bot_id, file_id
    );
    for _ in 0..4 {
        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("download request error: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("redirect without Location header")?
                .to_string();
            // Defense-in-depth: the bearer token is re-attached on every
            // hop, so never follow a downgrade to plaintext. Loopback HTTP
            // is allowed for the test harness only.
            let parsed =
                reqwest::Url::parse(&location).map_err(|e| format!("bad redirect URL: {e}"))?;
            let loopback_http = parsed.scheme() == "http"
                && matches!(
                    parsed.host_str(),
                    Some("127.0.0.1") | Some("localhost") | Some("[::1]")
                );
            if parsed.scheme() != "https" && !loopback_http {
                return Err(format!(
                    "insecure redirect target refused: {}://{}",
                    parsed.scheme(),
                    parsed.host_str().unwrap_or("?")
                ));
            }
            url = location;
            continue;
        }
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        if let Some(len) = resp.content_length() {
            if len > max_bytes {
                return Err(format!(
                    "size exceeded: {} exceeds {}",
                    format_bytes(len),
                    format_bytes(max_bytes)
                ));
            }
        }
        let mut resp = resp;
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("body read error: {e}"))?
        {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > max_bytes {
                return Err(format!(
                    "size exceeded: {} exceeds {}",
                    format_bytes(body.len() as u64),
                    format_bytes(max_bytes)
                ));
            }
        }
        return Ok((body, content_type));
    }
    Err("too many redirects".into())
}

async fn download_attachment(
    adapter: &LineWorksAdapter,
    kind: &str,
    file_id: &str,
    file_name: Option<&str>,
) -> Attachment {
    match kind {
        "image" => {
            let filename = format!("lineworks_{file_id}.jpg");
            match fetch_attachment_bytes(adapter, file_id, IMAGE_MAX_DOWNLOAD).await {
                Ok((bytes, _ct)) => {
                    match tokio::task::spawn_blocking(move || resize_and_compress(&bytes)).await {
                        Ok(Ok((compressed, mime))) => match store::store_media(&compressed).await {
                            Some(path) => {
                                let ext = if mime == "image/gif" { "gif" } else { "jpg" };
                                Attachment {
                                    attachment_type: "image".into(),
                                    filename: format!("lineworks_{file_id}.{ext}"),
                                    mime_type: mime,
                                    data: String::new(),
                                    size: compressed.len() as u64,
                                    path: Some(path),
                                    status: None,
                                }
                            }
                            None => Attachment::rejected(
                                "image",
                                filename,
                                "image/jpeg",
                                0,
                                "processing failed: storage error",
                            ),
                        },
                        _ => Attachment::rejected(
                            "image",
                            filename,
                            "image/jpeg",
                            0,
                            "processing failed: image encoding error",
                        ),
                    }
                }
                Err(reason) => {
                    warn!(file_id, %reason, "lineworks image download failed");
                    let reason = if reason.starts_with("size exceeded") {
                        reason
                    } else {
                        format!("download failed: {reason}")
                    };
                    Attachment::rejected("image", filename, "image/jpeg", 0, reason)
                }
            }
        }
        "audio" => {
            let fallback_name = format!("lineworks_{file_id}.audio");
            match fetch_attachment_bytes(adapter, file_id, AUDIO_MAX_DOWNLOAD).await {
                Ok((bytes, ct)) => match store::store_media(&bytes).await {
                    Some(path) => {
                        let ext = audio_extension(&ct);
                        Attachment {
                            attachment_type: "audio".into(),
                            filename: format!("lineworks_{file_id}.{ext}"),
                            mime_type: ct,
                            data: String::new(),
                            size: bytes.len() as u64,
                            path: Some(path),
                            status: None,
                        }
                    }
                    None => Attachment::rejected(
                        "audio",
                        fallback_name,
                        "audio/ogg",
                        0,
                        "processing failed: storage error",
                    ),
                },
                Err(reason) => {
                    warn!(file_id, %reason, "lineworks audio download failed");
                    let reason = if reason.starts_with("size exceeded") {
                        reason
                    } else {
                        format!("download failed: {reason}")
                    };
                    Attachment::rejected("audio", fallback_name, "audio/ogg", 0, reason)
                }
            }
        }
        // "file": only whitelisted text extensions are forwarded — binaries
        // have no representation the agent can consume.
        _ => {
            let filename = file_name
                .map(str::to_string)
                .unwrap_or_else(|| format!("lineworks_{file_id}"));
            if !is_text_extension(&filename) {
                return Attachment::rejected(
                    "text_file",
                    filename,
                    "application/octet-stream",
                    0,
                    "unsupported format: only text files are supported",
                );
            }
            match fetch_attachment_bytes(adapter, file_id, FILE_MAX_DOWNLOAD).await {
                Ok((bytes, _ct)) => match store::store_media(&bytes).await {
                    Some(path) => Attachment {
                        attachment_type: "text_file".into(),
                        filename,
                        mime_type: "text/plain".into(),
                        data: String::new(),
                        size: bytes.len() as u64,
                        path: Some(path),
                        status: None,
                    },
                    None => Attachment::rejected(
                        "text_file",
                        filename,
                        "text/plain",
                        0,
                        "processing failed: storage error",
                    ),
                },
                Err(reason) => {
                    warn!(file_id, %reason, "lineworks file download failed");
                    let reason = if reason.starts_with("size exceeded") {
                        reason
                    } else {
                        format!("download failed: {reason}")
                    };
                    Attachment::rejected("text_file", filename, "text/plain", 0, reason)
                }
            }
        }
    }
}

/// A media reference extracted during classification; downloaded only after
/// the event passes the mention gate (never before authorization).
#[derive(Debug, Clone)]
struct PendingAttachment {
    kind: String,
    file_id: String,
    file_name: Option<String>,
}

/// Cheap, pure event classification — no network I/O. Media events return a
/// [`PendingAttachment`] descriptor; the download happens post-gate in the
/// bounded worker.
fn classify_event(event: &LineWorksEvent) -> Option<(GatewayEvent, Option<PendingAttachment>)> {
    if event.event_type != "message" {
        return None;
    }
    let content = event.content.as_ref()?;
    let source = event.source.as_ref()?;

    let user_id = match source.user_id.as_deref() {
        Some(id) if !id.is_empty() => id,
        _ => {
            warn!("lineworks message event missing userId, skipping");
            return None;
        }
    };

    // Group talk events carry channelId; 1:1 events do not. The user: prefix
    // lets reply dispatch pick the users vs channels send endpoint from the
    // otherwise-opaque channel id.
    let (channel_id, channel_type) = match source.channel_id.as_deref() {
        Some(id) if !id.is_empty() => (id.to_string(), "channel".to_string()),
        _ => (
            format!("{USER_CHANNEL_PREFIX}{user_id}"),
            "user".to_string(),
        ),
    };

    let (text, pending) = match content.content_type.as_str() {
        "text" => {
            let text = content.text.as_deref().unwrap_or("");
            if text.trim().is_empty() {
                return None;
            }
            (text, None)
        }
        kind @ ("image" | "file" | "audio") => (
            "",
            Some(PendingAttachment {
                kind: kind.to_string(),
                file_id: content.file_id.as_deref().unwrap_or("unknown").to_string(),
                file_name: content.file_name.clone(),
            }),
        ),
        other => {
            info!(kind = %other, "lineworks: ignoring unsupported message content type");
            return None;
        }
    };

    let mut gateway_event = GatewayEvent::new(
        "lineworks",
        ChannelInfo {
            id: channel_id,
            channel_type,
            thread_id: None,
        },
        SenderInfo {
            id: user_id.into(),
            name: user_id.into(),
            display_name: user_id.into(),
            is_bot: false,
        },
        text,
        // Callback events carry no platform message id; reuse the event id.
        "",
        vec![],
    );
    gateway_event.message_id = gateway_event.event_id.clone();
    Some((gateway_event, pending))
}

// --- Reply dispatch ---

/// Split text into chunks of at most `limit` chars (char-boundary safe).
/// Linear: the running char count is tracked incrementally instead of
/// recounting the chunk per character.
fn split_text(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    for ch in text.chars() {
        if current_chars >= limit {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(ch);
        current_chars += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn send_endpoint(api_base: &str, bot_id: &str, channel_id: &str) -> String {
    match channel_id.strip_prefix(USER_CHANNEL_PREFIX) {
        Some(user_id) => format!("{api_base}/bots/{bot_id}/users/{user_id}/messages"),
        None => format!("{api_base}/bots/{bot_id}/channels/{channel_id}/messages"),
    }
}

pub async fn dispatch_lineworks_reply(
    client: &reqwest::Client,
    adapter: &LineWorksAdapter,
    reply: &GatewayReply,
) -> bool {
    // No reactions, threads, or edit/delete on LINE WORKS. Edit/delete are the
    // streaming path's cosmetic commands (see dispatch_line_reply for the full
    // rationale) — the final content still arrives as a plain send.
    if matches!(
        reply.command.as_deref(),
        Some("add_reaction")
            | Some("remove_reaction")
            | Some("create_topic")
            | Some("edit_message")
            | Some("delete_message")
    ) {
        info!(command = ?reply.command.as_deref(), "lineworks: ignoring unsupported command");
        return false;
    }

    if reply.content.text.trim().is_empty() {
        return false;
    }

    let url = send_endpoint(
        &adapter.config.api_base,
        &adapter.config.bot_id,
        &reply.channel.id,
    );

    // Flex first: markdown replies render as a flexible template when they
    // fit in one message. Any failure falls through to plain text so a
    // renderer or API quirk never loses content.
    if adapter.config.rich_messages && reply.content.text.chars().count() <= LINEWORKS_TEXT_LIMIT {
        if let Some((alt_text, bubble)) =
            super::lineworks_flex::markdown_to_flex(&reply.content.text)
        {
            let body = serde_json::json!({
                "content": {"type": "flex", "altText": alt_text, "contents": bubble}
            });
            if send_body(client, adapter, &url, &body).await {
                info!(to = %reply.channel.id, kind = "flex", "gateway → lineworks");
                return true;
            }
            warn!(to = %reply.channel.id, "lineworks flex send failed, falling back to text");
        }
    }

    let mut all_ok = true;
    for chunk in split_text(&reply.content.text, LINEWORKS_TEXT_LIMIT) {
        let body = serde_json::json!({"content": {"type": "text", "text": chunk}});
        if !send_body(client, adapter, &url, &body).await {
            all_ok = false;
            break;
        }
    }
    if all_ok {
        info!(to = %reply.channel.id, "gateway → lineworks");
    }
    all_ok
}

/// Send one message body; on 401, invalidate the cached token and retry once.
async fn send_body(
    client: &reqwest::Client,
    adapter: &LineWorksAdapter,
    url: &str,
    body: &serde_json::Value,
) -> bool {
    for attempt in 0..2 {
        let token = match adapter.token_cache.get_token(client, &adapter.config).await {
            Ok(t) => t,
            Err(e) => {
                error!(err = %e, "lineworks: cannot obtain access token");
                return false;
            }
        };
        let resp = client.post(url).bearer_auth(&token).json(body).send().await;
        match resp {
            Ok(r) if r.status().is_success() => return true,
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 => {
                warn!("lineworks send got 401, refreshing token and retrying");
                adapter.token_cache.invalidate_if(&token).await;
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                error!(status = %status, body = %body, "lineworks send error");
                return false;
            }
            Err(e) => {
                error!(err = %e, "lineworks send network error");
                return false;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    // Throwaway RSA key generated for these tests only (not a real credential).
    const TEST_RSA_KEY: &str = include_str!("../../testdata/lineworks_test_key.pem");

    fn test_config(api_base: &str, auth_base: &str) -> LineWorksConfig {
        LineWorksConfig {
            bot_id: "12345".into(),
            bot_secret: "test_bot_secret".into(),
            client_id: "test_client_id".into(),
            client_secret: "test_client_secret".into(),
            service_account: "sa@example.serviceaccount".into(),
            private_key: TEST_RSA_KEY.into(),
            webhook_path: "/webhook/lineworks".into(),
            api_base: api_base.into(),
            auth_base: auth_base.into(),
            require_mention: true,
            bot_name: Some("TestBot".into()),
            rich_messages: true,
            ack_message: None,
        }
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }

    async fn mount_token_endpoint(server: &MockServer, token: &str) -> wiremock::MockGuard {
        Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": token,
                "token_type": "Bearer",
                "expires_in": 86400,
                "scope": "bot"
            })))
            .mount_as_scoped(server)
            .await
    }

    // --- Token provider ---

    #[tokio::test]
    async fn token_request_shape_and_caching() {
        let server = MockServer::start().await;
        let _guard = mount_token_endpoint(&server, "tok_1").await;

        let config = test_config(&server.uri(), &server.uri());
        let cache = LineWorksTokenCache::new();
        let client = reqwest::Client::new();

        let tok = cache.get_token(&client, &config).await.unwrap();
        assert_eq!(tok, "tok_1");
        // Second call is served from cache — still exactly one HTTP request.
        let tok2 = cache.get_token(&client, &config).await.unwrap();
        assert_eq!(tok2, "tok_1");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);

        let form: std::collections::HashMap<String, String> = url_decoded_form(&requests[0]);
        assert_eq!(
            form["grant_type"],
            "urn:ietf:params:oauth:grant-type:jwt-bearer"
        );
        assert_eq!(form["client_id"], "test_client_id");
        assert_eq!(form["client_secret"], "test_client_secret");
        assert_eq!(
            form["scope"], "bot.message,bot.read",
            "least-privilege scope contract"
        );

        // The assertion must be a 3-part RS256 JWT with our iss/sub claims.
        let jwt = &form["assertion"];
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        use base64::Engine;
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "test_client_id");
        assert_eq!(claims["sub"], "sa@example.serviceaccount");
        assert!(claims["exp"].as_u64().unwrap() > claims["iat"].as_u64().unwrap());
    }

    #[tokio::test]
    async fn token_invalidate_forces_refetch() {
        let server = MockServer::start().await;
        let _guard = mount_token_endpoint(&server, "tok_a").await;

        let config = test_config(&server.uri(), &server.uri());
        let cache = LineWorksTokenCache::new();
        let client = reqwest::Client::new();

        let tok = cache.get_token(&client, &config).await.unwrap();
        cache.invalidate_if(&tok).await;
        cache.get_token(&client, &config).await.unwrap();

        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delayed_401_does_not_clear_refreshed_token() {
        // Interleaving under test: task A sends with tok_old → another task
        // refreshes the cache to tok_new → A's DELAYED 401 arrives and
        // invalidates. The invalidation must be a no-op (tok_new survives)
        // and the next get_token must not hit the token endpoint at all.
        // The barrier guarantees the refresh is published before the delayed
        // invalidation runs.
        let cache = Arc::new(LineWorksTokenCache::new());
        *cache.token.write().await = Some(("tok_old".to_string(), Instant::now(), 3600));

        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let refresher = {
            let cache = cache.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                // Concurrent task refreshes the token (as a successful
                // get_token re-exchange would).
                *cache.token.write().await =
                    Some(("tok_new".to_string(), Instant::now(), 3600));
                barrier.wait().await;
            })
        };
        let delayed_401 = {
            let cache = cache.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                // Delayed 401 from the OLD token arrives after the refresh.
                cache.invalidate_if("tok_old").await;
            })
        };
        refresher.await.unwrap();
        delayed_401.await.unwrap();

        // tok_new must survive; an unroutable auth base proves get_token
        // serves it from cache without a serialized re-exchange.
        let config = test_config("http://unused", "http://127.0.0.1:1");
        let tok = cache
            .get_token(&reqwest::Client::new(), &config)
            .await
            .expect("newer token must survive a delayed stale 401");
        assert_eq!(tok, "tok_new");

        // A 401 from the CURRENT token still clears the cache.
        cache.invalidate_if("tok_new").await;
        assert!(cache.token.read().await.is_none());
    }

    fn url_decoded_form(req: &Request) -> std::collections::HashMap<String, String> {
        let body = String::from_utf8(req.body.clone()).unwrap();
        body.split('&')
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                Some((
                    urlencoding::decode(k).ok()?.into_owned(),
                    urlencoding::decode(v).ok()?.into_owned(),
                ))
            })
            .collect()
    }

    // --- Signature verification ---

    #[test]
    fn signature_accept_and_reject() {
        let body = br#"{"type":"message"}"#;
        let good = sign("secret_x", body);
        assert!(verify_signature("secret_x", body, &good));
        assert!(!verify_signature("secret_x", body, "AAAA"));
        assert!(!verify_signature("wrong_secret", body, &good));
        // Signature over different body must not verify.
        assert!(!verify_signature("secret_x", b"other body", &good));
    }

    // --- Event mapping ---

    fn classify(json: serde_json::Value) -> Option<(GatewayEvent, Option<PendingAttachment>)> {
        let event: LineWorksEvent = serde_json::from_value(json).unwrap();
        classify_event(&event)
    }

    async fn parse_event(json: serde_json::Value) -> Option<GatewayEvent> {
        classify(json).map(|(ev, _)| ev)
    }

    #[tokio::test]
    async fn maps_direct_text_message() {
        let ev = parse_event(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1", "domainId": 1},
            "issuedTime": "2026-07-24T10:00:00Z",
            "content": {"type": "text", "text": "hello"}
        }))
        .await
        .expect("text message should map");
        assert_eq!(ev.platform, "lineworks");
        assert_eq!(ev.channel.id, "user:U1");
        assert_eq!(ev.channel.channel_type, "user");
        assert_eq!(ev.sender.id, "U1");
        assert_eq!(ev.content.text, "hello");
        assert_eq!(ev.message_id, ev.event_id);
    }

    #[tokio::test]
    async fn maps_channel_text_message() {
        let ev = parse_event(serde_json::json!({
            "type": "message",
            "source": {"userId": "U2", "channelId": "C9", "domainId": 1},
            "content": {"type": "text", "text": "hi all"}
        }))
        .await
        .expect("channel message should map");
        assert_eq!(ev.channel.id, "C9");
        assert_eq!(ev.channel.channel_type, "channel");
        assert_eq!(ev.sender.id, "U2");
    }

    #[test]
    fn classification_is_pure_and_defers_attachment_download() {
        // Media events classify to a pending descriptor — no I/O happens here.
        let (ev, pending) = classify(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "image", "fileId": "F123"}
        }))
        .expect("image should classify");
        assert_eq!(ev.content.text, "");
        assert!(
            ev.content.attachments.is_empty(),
            "no download at classify time"
        );
        let p = pending.expect("media event carries a pending attachment");
        assert_eq!(p.kind, "image");
        assert_eq!(p.file_id, "F123");
        // Text events carry no pending work.
        let (_ev, pending) = classify(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "hi"}
        }))
        .unwrap();
        assert!(pending.is_none());
        // Empty text is dropped at classification.
        assert!(classify(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "   "}
        }))
        .is_none());
    }

    #[tokio::test]
    async fn ignores_non_message_and_unsupported_events() {
        assert!(parse_event(serde_json::json!({
            "type": "join",
            "source": {"userId": "U1", "channelId": "C1"}
        }))
        .await
        .is_none());
        assert!(parse_event(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "sticker", "packageId": "1", "stickerId": "2"}
        }))
        .await
        .is_none());
        // Missing userId → skip
        assert!(parse_event(serde_json::json!({
            "type": "message",
            "source": {"channelId": "C1"},
            "content": {"type": "text", "text": "x"}
        }))
        .await
        .is_none());
    }

    // --- Attachment download ---

    /// Mount the 302-redirect download flow: attachments endpoint redirects
    /// to /storage/{fileId}, which serves `bytes` with `content_type`.
    async fn mount_attachment_download(
        server: &MockServer,
        file_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> (wiremock::MockGuard, wiremock::MockGuard) {
        let redirect = Mock::given(method("GET"))
            .and(path(format!("/bots/12345/attachments/{file_id}")))
            .and(wiremock::matchers::header("authorization", "Bearer tok_dl"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/storage/{file_id}", server.uri())),
            )
            .expect(1)
            .mount_as_scoped(server)
            .await;
        let storage = Mock::given(method("GET"))
            .and(path(format!("/storage/{file_id}")))
            .and(wiremock::matchers::header("authorization", "Bearer tok_dl"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", content_type)
                    .set_body_bytes(bytes),
            )
            .expect(1)
            .mount_as_scoped(server)
            .await;
        (redirect, storage)
    }

    #[tokio::test]
    async fn image_downloads_via_redirect_and_stores() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_dl").await;
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([0, 255, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let _guards =
            mount_attachment_download(&server, "F_img", buf.into_inner(), "image/png").await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let att = download_attachment(&adapter, "image", "F_img", None).await;
        let att = &att;
        assert_eq!(att.attachment_type, "image");
        assert!(
            att.status.is_none(),
            "download should succeed: {:?}",
            att.status
        );
        assert!(att.size > 0);
        let path = att.path.clone().expect("stored path");
        let stored = tokio::fs::read(&path).await.unwrap();
        assert!(!stored.is_empty());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn text_file_downloads_and_binary_rejected() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_dl").await;
        let _guards = mount_attachment_download(
            &server,
            "F_log",
            b"line one\nline two".to_vec(),
            "text/plain",
        )
        .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));

        // Whitelisted text extension downloads and stores.
        let att = download_attachment(&adapter, "file", "F_log", Some("build.log")).await;
        let att = &att;
        assert_eq!(att.attachment_type, "text_file");
        assert_eq!(att.filename, "build.log");
        assert!(
            att.status.is_none(),
            "text file should download: {:?}",
            att.status
        );
        if let Some(p) = att.path.clone() {
            let _ = tokio::fs::remove_file(p).await;
        }

        // Binary extension is rejected without hitting the download API.
        let att = download_attachment(&adapter, "file", "F_bin", Some("tool.exe")).await;
        assert!(att
            .status
            .as_deref()
            .unwrap()
            .starts_with("unsupported format"));
    }

    #[tokio::test]
    async fn insecure_redirect_target_refused() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_dl").await;
        // Redirect to a non-loopback plaintext host must be refused before
        // the bearer token is forwarded.
        let _redirect = Mock::given(method("GET"))
            .and(path("/bots/12345/attachments/F_http"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://storage.example.com/F_http"),
            )
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let err = fetch_attachment_bytes(&adapter, "F_http", 1024)
            .await
            .expect_err("plaintext redirect must be refused");
        assert!(
            err.starts_with("insecure redirect target refused"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn oversized_download_rejected() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_dl").await;
        let _big = Mock::given(method("GET"))
            .and(path("/bots/12345/attachments/F_big"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0u8; 64]),
            )
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let err = fetch_attachment_bytes(&adapter, "F_big", 16)
            .await
            .expect_err("64-byte body must exceed the 16-byte cap");
        assert!(err.starts_with("size exceeded"), "unexpected error: {err}");
    }

    // --- Webhook handler (HTTP-level) ---

    fn test_state(
        adapter: Option<LineWorksAdapter>,
    ) -> (Arc<crate::AppState>, broadcast::Receiver<String>) {
        let (event_tx, event_rx) = broadcast::channel(16);
        let mut state = crate::AppState::test_default(event_tx);
        state.lineworks = adapter.map(Arc::new);
        (Arc::new(state), event_rx)
    }

    #[tokio::test]
    async fn webhook_rejects_bad_signature_and_accepts_good() {
        let config = test_config("http://unused", "http://unused");
        let secret = config.bot_secret.clone();
        let (state, mut event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "ping"}
        })
        .to_string();

        // Missing signature
        let status = webhook(
            State(state.clone()),
            axum::http::HeaderMap::new(),
            axum::body::Bytes::from(body.clone()),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        // Bad signature
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-works-signature", "bm9wZQ==".parse().unwrap());
        let status = webhook(
            State(state.clone()),
            headers,
            axum::body::Bytes::from(body.clone()),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        // Good signature but MISSING X-WORKS-BotId → rejected (required header).
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        let status = webhook(
            State(state.clone()),
            headers,
            axum::body::Bytes::from(body.clone()),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        // Good signature + bot id → 200 and event broadcast (async worker).
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);

        let event_json = recv_event(&mut event_rx)
            .await
            .expect("event should be broadcast");
        let ev: GatewayEvent = serde_json::from_str(&event_json).unwrap();
        assert_eq!(ev.platform, "lineworks");
        assert_eq!(ev.content.text, "ping");
    }

    /// The webhook acks before processing; wait briefly for the async worker
    /// to broadcast.
    async fn recv_event(rx: &mut broadcast::Receiver<String>) -> Option<String> {
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    #[tokio::test]
    async fn webhook_refuses_ack_without_event_consumer() {
        let config = test_config("http://unused", "http://unused");
        let secret = config.bot_secret.clone();
        let (state, event_rx) = test_state(Some(LineWorksAdapter::new(config)));
        // No consumer attached → acking would silently lose the event.
        drop(event_rx);

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "ping"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn webhook_acks_before_slow_attachment_download() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_slow").await;
        // Attachment endpoint stalls for 5s — far longer than the ack budget.
        let _slow = Mock::given(method("GET"))
            .and(path("/bots/12345/attachments/F_slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![0u8; 8])
                    .set_delay(std::time::Duration::from_secs(5)),
            )
            .mount_as_scoped(&server)
            .await;

        let config = test_config(&server.uri(), &server.uri());
        let secret = config.bot_secret.clone();
        let (state, _event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "image", "fileId": "F_slow"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());

        let started = std::time::Instant::now();
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "callback must ack before attachment I/O completes (took {:?})",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn untrusted_sender_never_downloads_attachment() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_trust").await;
        // Deny-all probe: the download endpoint must never be hit.
        let _attachment = Mock::given(method("GET"))
            .and(path("/bots/12345/attachments/F_untrusted"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 8]))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let config = test_config(&server.uri(), &server.uri());
        let secret = config.bot_secret.clone();
        let (state, mut event_rx) = {
            let (event_tx, event_rx) = broadcast::channel(16);
            let mut state = crate::AppState::test_default(event_tx);
            state.lineworks = Some(Arc::new(LineWorksAdapter::new(config)));
            state.trust_probe = Some(Arc::new(|_platform, _channel, _sender| false));
            (Arc::new(state), event_rx)
        };

        // 1:1 image event: passes the mention gate, fails the trust probe.
        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U_untrusted"},
            "content": {"type": "image", "fileId": "F_untrusted"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);

        // The event is still broadcast (core deny-echo path needs it) with a
        // security-rejected attachment instead of downloaded content.
        let event_json = recv_event(&mut event_rx)
            .await
            .expect("event still broadcast");
        let ev: GatewayEvent = serde_json::from_str(&event_json).unwrap();
        let att = &ev.content.attachments[0];
        assert!(att
            .status
            .as_deref()
            .unwrap()
            .starts_with("security rejected"));
        assert!(att.path.is_none());
    }

    #[tokio::test]
    async fn gated_channel_message_never_downloads_attachment() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_gate").await;
        // Expect ZERO hits on the attachment endpoint: the unmentioned
        // channel message must be dropped before any download.
        let _attachment = Mock::given(method("GET"))
            .and(path("/bots/12345/attachments/F_gated"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 8]))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let config = test_config(&server.uri(), &server.uri());
        let secret = config.bot_secret.clone();
        let (state, mut event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        // Channel (group) image event without any mention possibility:
        // image events have no text, so the mention gate drops them.
        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1", "channelId": "C1", "domainId": 1},
            "content": {"type": "image", "fileId": "F_gated"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        // No event may be broadcast, and the scoped mock's expect(0) verifies
        // no download happened when the guard drops.
        assert!(recv_event(&mut event_rx).await.is_none());
    }

    #[tokio::test]
    async fn webhook_sends_ack_message_when_configured() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_ack").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = test_config(&server.uri(), &server.uri());
        config.ack_message = Some("🤔 處理中…".into());
        let secret = config.bot_secret.clone();
        let (state, mut event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "ping"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        // The event is still broadcast to the agent path (async worker).
        assert!(recv_event(&mut event_rx).await.is_some());

        // The ack lands asynchronously; poll briefly for the mock hit.
        for _ in 0..40 {
            if !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().contains("/messages"))
            {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
        let ack_req = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.url.path().contains("/messages"))
            .expect("ack message should be sent");
        let ack_body: serde_json::Value = serde_json::from_slice(&ack_req.body).unwrap();
        assert_eq!(ack_body["content"]["text"], "🤔 處理中…");
    }

    #[tokio::test]
    async fn webhook_rejects_bot_id_mismatch() {
        let config = test_config("http://unused", "http://unused");
        let secret = config.bot_secret.clone();
        let (state, _event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        let body = r#"{"type":"message"}"#.to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "99999".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }

    // --- Mention gating ---

    async fn channel_text_event(text: &str) -> GatewayEvent {
        parse_event(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1", "channelId": "C1", "domainId": 1},
            "content": {"type": "text", "text": text}
        }))
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn mention_gate_drops_unmentioned_channel_message() {
        let adapter = LineWorksAdapter::new(test_config("http://unused", "http://unused"));
        let client = reqwest::Client::new();
        let mut ev = channel_text_event("hello without mention").await;
        assert!(!passes_mention_gate(&adapter, &client, &mut ev).await);
    }

    #[tokio::test]
    async fn mention_gate_passes_and_strips_mention() {
        let adapter = LineWorksAdapter::new(test_config("http://unused", "http://unused"));
        let client = reqwest::Client::new();
        let mut ev = channel_text_event("@TestBot 幫我查一下").await;
        assert!(passes_mention_gate(&adapter, &client, &mut ev).await);
        assert_eq!(ev.content.text, "幫我查一下");
    }

    #[tokio::test]
    async fn mention_gate_skips_dm_and_disabled() {
        let client = reqwest::Client::new();
        // 1:1 always passes
        let adapter = LineWorksAdapter::new(test_config("http://unused", "http://unused"));
        let mut dm = parse_event(serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "no mention"}
        }))
        .await
        .unwrap();
        assert!(passes_mention_gate(&adapter, &client, &mut dm).await);
        // require_mention = false passes channel messages untouched
        let mut config = test_config("http://unused", "http://unused");
        config.require_mention = false;
        let adapter = LineWorksAdapter::new(config);
        let mut ev = channel_text_event("ambient message").await;
        assert!(passes_mention_gate(&adapter, &client, &mut ev).await);
        assert_eq!(ev.content.text, "ambient message");
    }

    #[tokio::test]
    async fn mention_gate_fetches_bot_name_from_api() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_name").await;
        let _bot = Mock::given(method("GET"))
            .and(path("/bots/12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "botId": 12345,
                "botName": "Nuphos (Dev)"
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = test_config(&server.uri(), &server.uri());
        config.bot_name = None; // force API lookup
        let adapter = LineWorksAdapter::new(config);
        let client = reqwest::Client::new();

        let mut ev = channel_text_event("@Nuphos (Dev) hi").await;
        assert!(passes_mention_gate(&adapter, &client, &mut ev).await);
        assert_eq!(ev.content.text, "hi");
        // Second call served from cache (expect(1) enforces a single fetch).
        let mut ev2 = channel_text_event("no mention here").await;
        assert!(!passes_mention_gate(&adapter, &client, &mut ev2).await);
    }

    // --- Round-3 regression tests ---

    #[test]
    fn mention_match_is_boundary_aware() {
        // "@Bot" must not match inside "@Bottage".
        assert!(strip_boundary_mention("@Bottage hello", "@Bot").is_none());
        // Word boundary / punctuation / end-of-text all match.
        assert_eq!(
            strip_boundary_mention("@Bot hello", "@Bot").as_deref(),
            Some("hello")
        );
        assert_eq!(
            strip_boundary_mention("hey @Bot!", "@Bot").as_deref(),
            Some("hey !")
        );
        assert_eq!(
            strip_boundary_mention("ping @Bot", "@Bot").as_deref(),
            Some("ping")
        );
        // Non-boundary text is left untouched while a later boundary match strips.
        assert_eq!(
            strip_boundary_mention("@Bottage and @Bot do it", "@Bot").as_deref(),
            Some("@Bottage and  do it")
        );
        // Names with spaces/parens work as literal strings.
        assert_eq!(
            strip_boundary_mention("@Nuphos (Dev) hi", "@Nuphos (Dev)").as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn invalid_pem_disables_adapter() {
        let read = |k: &str| -> Option<String> {
            match k {
                "LINEWORKS_BOT_ID" => Some("1".into()),
                "LINEWORKS_BOT_SECRET" => Some("s".into()),
                "LINEWORKS_CLIENT_ID" => Some("c".into()),
                "LINEWORKS_CLIENT_SECRET" => Some("cs".into()),
                "LINEWORKS_SERVICE_ACCOUNT" => Some("sa@x".into()),
                "LINEWORKS_PRIVATE_KEY" => Some("not a pem".into()),
                _ => None,
            }
        };
        assert!(LineWorksConfig::from_reader(read).is_none());
        assert!(!valid_private_key("not a pem"));
        assert!(valid_private_key(include_str!(
            "../../testdata/lineworks_test_key.pem"
        )));
    }

    #[tokio::test]
    async fn saturated_workers_queue_callback_then_drain() {
        let config = test_config("http://unused", "http://unused");
        let secret = config.bot_secret.clone();
        let (state, mut event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        // Exhaust every worker permit — the callback must still be ACCEPTED
        // (queued for burst absorption), not rejected: LINE WORKS does not
        // resend callbacks, so a 503 here would permanently drop the message.
        let held: Vec<_> = (0..crate::LINEWORKS_WEBHOOK_CONCURRENCY_MAX)
            .map(|_| {
                state
                    .lineworks_webhook_semaphore
                    .clone()
                    .try_acquire_owned()
                    .unwrap()
            })
            .collect();

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "ping"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state.clone()), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::OK);

        // While every worker is busy the queued event must not process yet.
        assert!(
            event_rx.try_recv().is_err(),
            "queued event must wait for a worker permit"
        );

        // Free the workers — the queued event must drain and broadcast.
        drop(held);
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("queued event must be processed after a worker frees")
            .expect("broadcast channel must stay open");
        assert!(
            received.contains("ping"),
            "drained event must carry the message"
        );
    }

    #[tokio::test]
    async fn ingress_queue_overflow_returns_503() {
        let config = test_config("http://unused", "http://unused");
        let secret = config.bot_secret.clone();
        let (state, _event_rx) = test_state(Some(LineWorksAdapter::new(config)));

        // Fill the entire ingress queue — only then may the full policy
        // answer 503 (loud, bounded overflow).
        let _queued: Vec<_> = (0..crate::LINEWORKS_INGRESS_QUEUE_MAX)
            .map(|_| {
                state
                    .lineworks_ingress_queue
                    .clone()
                    .try_acquire_owned()
                    .unwrap()
            })
            .collect();

        let body = serde_json::json!({
            "type": "message",
            "source": {"userId": "U1"},
            "content": {"type": "text", "text": "ping"}
        })
        .to_string();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-works-signature",
            sign(&secret, body.as_bytes()).parse().unwrap(),
        );
        headers.insert("x-works-botid", "12345".parse().unwrap());
        let status = webhook(State(state), headers, axum::body::Bytes::from(body)).await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn token_endpoint_http_error_is_surfaced() {
        let server = MockServer::start().await;
        let _bad = Mock::given(method("POST"))
            .and(path("/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream sad"))
            .mount_as_scoped(&server)
            .await;
        let config = test_config(&server.uri(), &server.uri());
        let cache = LineWorksTokenCache::new();
        let err = cache
            .get_token(&reqwest::Client::new(), &config)
            .await
            .expect_err("HTTP 503 must be an error");
        assert!(
            err.contains("token endpoint HTTP 503"),
            "status must be surfaced, got: {err}"
        );
    }

    #[tokio::test]
    async fn failed_bot_name_lookup_is_negatively_cached() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_nc").await;
        // Bot-info endpoint fails; it must be hit exactly once — the second
        // gate call inside the cooldown window returns fast without I/O.
        let _bot = Mock::given(method("GET"))
            .and(path("/bots/12345"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut config = test_config(&server.uri(), &server.uri());
        config.bot_name = None; // force API lookup
        let adapter = LineWorksAdapter::new(config);
        let client = reqwest::Client::new();
        assert!(adapter.bot_name(&client).await.is_none());
        assert!(adapter.bot_name(&client).await.is_none());
    }

    // --- Reply dispatch ---

    fn text_reply(channel_id: &str, text: &str, command: Option<&str>) -> GatewayReply {
        GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "evt_1".into(),
            platform: "lineworks".into(),
            channel: ReplyChannel {
                id: channel_id.into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                text: text.into(),
                attachments: vec![],
            },
            command: command.map(Into::into),
            request_id: None,
            quote_message_id: None,
        }
    }

    #[tokio::test]
    async fn dispatch_selects_user_endpoint_and_sends_text() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_send").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer tok_send",
            ))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", "hello back", None),
        )
        .await;
        assert!(ok);

        let sent = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.url.path().contains("/messages"))
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["content"]["type"], "text");
        assert_eq!(body["content"]["text"], "hello back");
    }

    #[tokio::test]
    async fn dispatch_selects_channel_endpoint() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_send").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/channels/C9/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("C9", "to the channel", None),
        )
        .await;
        assert!(ok);
    }

    #[tokio::test]
    async fn dispatch_splits_long_text() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_send").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(2)
            .mount_as_scoped(&server)
            .await;

        let long_text = "あ".repeat(LINEWORKS_TEXT_LIMIT + 1);
        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", &long_text, None),
        )
        .await;
        assert!(ok);
    }

    #[tokio::test]
    async fn dispatch_ignores_unsupported_commands() {
        // No mock server: any HTTP call would fail the test via ok == false.
        let adapter = LineWorksAdapter::new(test_config("http://unused", "http://unused"));
        for cmd in [
            "add_reaction",
            "remove_reaction",
            "create_topic",
            "edit_message",
            "delete_message",
        ] {
            let ok = dispatch_lineworks_reply(
                &reqwest::Client::new(),
                &adapter,
                &text_reply("user:U1", "x", Some(cmd)),
            )
            .await;
            assert!(!ok, "command {cmd} should be a no-op");
        }
    }

    #[tokio::test]
    async fn dispatch_sends_flex_for_markdown_reply() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_flex").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", "# 報告\n- 第一點\n- 第二點", None),
        )
        .await;
        assert!(ok);

        let sent = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.url.path().contains("/messages"))
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["content"]["type"], "flex");
        assert!(!body["content"]["altText"].as_str().unwrap().is_empty());
        assert_eq!(body["content"]["contents"]["type"], "bubble");
    }

    #[tokio::test]
    async fn dispatch_falls_back_to_text_when_flex_rejected() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_fb").await;
        // First request (flex) → 400; the retry as plain text → 201.
        let _flex = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(400))
            .up_to_n_times(1)
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _text = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", "# 標題\n內容", None),
        )
        .await;
        assert!(ok);

        let bodies: Vec<serde_json::Value> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path().contains("/messages"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["content"]["type"], "flex");
        assert_eq!(bodies[1]["content"]["type"], "text");
    }

    #[tokio::test]
    async fn dispatch_plain_text_skips_flex() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_plain").await;
        let _send = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", "純文字回覆，沒有任何排版", None),
        )
        .await;
        assert!(ok);

        let body: serde_json::Value = serde_json::from_slice(
            &server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.url.path().contains("/messages"))
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(body["content"]["type"], "text");
    }

    #[tokio::test]
    async fn dispatch_retries_once_after_401() {
        let server = MockServer::start().await;
        let _token = mount_token_endpoint(&server, "tok_x").await;
        // First send → 401, second send → 201.
        let _first = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _second = Mock::given(method("POST"))
            .and(path("/bots/12345/users/U1/messages"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let adapter = LineWorksAdapter::new(test_config(&server.uri(), &server.uri()));
        let ok = dispatch_lineworks_reply(
            &reqwest::Client::new(),
            &adapter,
            &text_reply("user:U1", "retry me", None),
        )
        .await;
        assert!(ok);

        // Token endpoint hit twice: initial fetch + post-401 refresh.
        let token_requests = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/oauth2/v2.0/token")
            .count();
        assert_eq!(token_requests, 2);
    }

    #[test]
    fn split_text_boundaries() {
        assert_eq!(split_text("", 5), Vec::<String>::new());
        assert_eq!(split_text("abc", 5), vec!["abc"]);
        assert_eq!(split_text("abcde", 5), vec!["abcde"]);
        assert_eq!(split_text("abcdef", 5), vec!["abcde", "f"]);
        // Multibyte chars split on char boundary, not bytes.
        let s = "你好世界你好";
        assert_eq!(split_text(s, 4), vec!["你好世界", "你好"]);
    }
}

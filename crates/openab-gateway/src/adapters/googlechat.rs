use crate::media::format_bytes;
use crate::schema::*;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

pub const GOOGLE_CHAT_API_BASE: &str = "https://chat.googleapis.com/v1";
const GOOGLE_CHAT_MESSAGE_LIMIT: usize = 4096;

const IMAGE_MAX_DIMENSION_PX: u32 = 1200;
const IMAGE_JPEG_QUALITY: u8 = 75;
const IMAGE_MAX_DOWNLOAD: u64 = 10 * 1024 * 1024; // 10 MB
const FILE_MAX_DOWNLOAD: u64 = 512 * 1024; // 512 KB
const AUDIO_MAX_DOWNLOAD: u64 = 25 * 1024 * 1024; // 25 MB
/// Per-request timeout for Google Chat Media API downloads. Prevents a hung
/// connection from blocking the spawned download task indefinitely.
const MEDIA_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Bound every token-mint request (SA-key exchange, metadata, IAM Credentials)
/// so a hung connection cannot stall senders queued behind the token cache's
/// write lock (the refresh runs while holding it) or prevent the ADC → static
/// token degradation path from engaging.
const TOKEN_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Cap on text file attachments per message (matches Discord/Slack).
const TEXT_FILE_COUNT_CAP: usize = 5;
/// Cap on aggregate text file bytes per message (matches Discord/Slack 1 MB).
const TEXT_TOTAL_CAP: u64 = 1024 * 1024;

// --- Google Chat types ---
//
// Google Chat delivers webhooks in two shapes depending on the App's
// Connection settings in the Cloud Console:
//   - HTTP endpoint URL mode: top-level fields (message, user, space, ...)
//   - Pub/Sub mode:           wrapped under `chat.messagePayload`
// Both are supported via the optional fields below; the handler prefers
// the wrapped form and falls back to top-level when `chat` is absent.

#[derive(Debug, Deserialize)]
pub struct GoogleChatEnvelope {
    pub chat: Option<ChatPayload>,
    // HTTP endpoint URL top-level fields (used when `chat` is None)
    pub message: Option<GoogleChatMessage>,
    pub user: Option<GoogleChatUser>,
    pub space: Option<GoogleChatSpace>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPayload {
    pub user: Option<GoogleChatUser>,
    pub message_payload: Option<MessagePayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePayload {
    pub message: Option<GoogleChatMessage>,
    pub space: Option<GoogleChatSpace>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleChatMessage {
    pub name: String,
    pub text: Option<String>,
    pub argument_text: Option<String>,
    pub sender: Option<GoogleChatUser>,
    pub thread: Option<GoogleChatThread>,
    pub space: Option<GoogleChatSpace>,
    #[serde(default)]
    pub attachment: Vec<GoogleChatAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleChatAttachment {
    #[allow(dead_code)]
    pub name: Option<String>,
    pub content_name: Option<String>,
    pub content_type: Option<String>,
    pub source: Option<String>,
    pub attachment_data_ref: Option<AttachmentDataRef>,
    #[allow(dead_code)]
    pub drive_data_ref: Option<DriveDataRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDataRef {
    pub resource_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct DriveDataRef {
    pub drive_file_id: Option<String>,
}

/// Reference to media that needs async download after webhook parse.
#[derive(Debug, Clone)]
pub enum GoogleChatMediaRef {
    Image {
        resource_name: String,
        content_name: String,
    },
    File {
        resource_name: String,
        content_name: String,
    },
    Audio {
        resource_name: String,
        content_name: String,
        content_type: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleChatUser {
    pub name: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub user_type: String,
}

#[derive(Debug, Deserialize)]
pub struct GoogleChatThread {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleChatSpace {
    pub name: String,
    #[serde(rename = "type")]
    pub space_type: Option<String>,
    // Parsed by serde, not consumed in current code paths.
    #[allow(dead_code)]
    pub space_type_renamed: Option<String>,
}

// --- Webhook JWT verification ---

const GOOGLE_CHAT_ISSUER: &str = "https://accounts.google.com";
const GOOGLE_CHAT_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const GOOGLE_CHAT_SIGNER_EMAIL: &str = "chat@system.gserviceaccount.com";
const JWKS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Verify the JWT's `email` claim belongs to Google Chat.
/// HTTP endpoint URL webhooks are signed by `chat@system.gserviceaccount.com`.
/// Without this check, any Google-issued ID token would be accepted.
fn verify_email_claim(claims: &serde_json::Value) -> Result<(), String> {
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or("missing email claim")?;
    if email != GOOGLE_CHAT_SIGNER_EMAIL {
        return Err(format!(
            "email claim mismatch: expected {GOOGLE_CHAT_SIGNER_EMAIL}, got {email}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct JwkKey {
    kid: Option<String>,
    n: String,
    e: String,
    kty: String,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

pub struct GoogleChatJwtVerifier {
    audience: String,
    client: reqwest::Client,
    jwks_cache: RwLock<Option<(Vec<JwkKey>, Instant)>>,
}

impl GoogleChatJwtVerifier {
    pub fn new(audience: String) -> Self {
        Self {
            audience,
            client: reqwest::Client::new(),
            jwks_cache: RwLock::new(None),
        }
    }

    async fn get_jwks(&self) -> Result<Vec<JwkKey>, String> {
        {
            let cache = self.jwks_cache.read().await;
            if let Some((ref keys, fetched_at)) = *cache {
                if fetched_at.elapsed() < JWKS_CACHE_TTL {
                    return Ok(keys.clone());
                }
            }
        }
        let jwks: JwksResponse = self
            .client
            .get(GOOGLE_CHAT_JWKS_URL)
            .send()
            .await
            .map_err(|e| format!("JWKS fetch error: {e}"))?
            .json()
            .await
            .map_err(|e| format!("JWKS parse error: {e}"))?;

        let keys = jwks.keys;
        *self.jwks_cache.write().await = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    pub async fn verify(&self, auth_header: &str) -> Result<(), String> {
        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or("missing Bearer prefix")?;

        let header =
            jsonwebtoken::decode_header(token).map_err(|e| format!("invalid JWT header: {e}"))?;
        let kid = header.kid.ok_or("no kid in JWT header")?;

        let keys = self.get_jwks().await?;
        let key = match keys.iter().find(|k| k.kid.as_deref() == Some(&kid)) {
            Some(k) => k.clone(),
            None => {
                // Key rotation: invalidate cache and retry
                *self.jwks_cache.write().await = None;
                let refreshed = self.get_jwks().await?;
                refreshed
                    .into_iter()
                    .find(|k| k.kid.as_deref() == Some(&kid))
                    .ok_or_else(|| format!("no matching JWK for kid={kid}"))?
            }
        };

        if key.kty != "RSA" {
            return Err(format!("unsupported key type: {}", key.kty));
        }

        let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)
            .map_err(|e| format!("RSA key decode error: {e}"))?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[GOOGLE_CHAT_ISSUER]);
        validation.validate_exp = true;

        let token_data = decode::<serde_json::Value>(token, &decoding_key, &validation)
            .map_err(|e| format!("JWT validation failed: {e}"))?;

        verify_email_claim(&token_data.claims)?;

        Ok(())
    }
}

// --- Adapter (encapsulates all Google Chat state) ---

pub struct GoogleChatAdapter {
    pub token_cache: Option<GoogleChatTokenCache>,
    pub metadata_source: Option<MetadataTokenSource>,
    pub access_token: Option<String>,
    pub jwt_verifier: Option<GoogleChatJwtVerifier>,
    pub client: reqwest::Client,
    pub api_base: String,
}

/// Named construction parts for [`GoogleChatAdapter::from_parts`], so call
/// sites name each field instead of counting five positional arguments.
#[derive(Default)]
pub(crate) struct GoogleChatParts {
    pub sa_key_json: Option<String>,
    pub sa_key_file: Option<String>,
    pub access_token: Option<String>,
    pub audience: Option<String>,
    pub use_adc: bool,
}

impl GoogleChatAdapter {
    /// Build an adapter from resolved parts (#1379): SA key JSON (inline wins
    /// over file path), optional static access token, optional JWT audience,
    /// and `use_adc` (keyless ADC via the GCE metadata server + IAM
    /// Credentials). Auth precedence at send time: SA key > ADC > static token.
    /// Shared by env-derived construction and `apply_googlechat_config`.
    pub(crate) fn from_parts(parts: GoogleChatParts) -> Self {
        use tracing::{info, warn};
        let GoogleChatParts {
            sa_key_json,
            sa_key_file,
            access_token,
            audience,
            use_adc,
        } = parts;
        let key_configured = sa_key_json.is_some() || sa_key_file.is_some();
        let token_cache = sa_key_json
            .or_else(|| {
                sa_key_file.and_then(|path| {
                    std::fs::read_to_string(&path)
                        .map_err(|e| {
                            warn!("failed to read Google Chat SA key file '{}': {e}", path);
                        })
                        .ok()
                })
            })
            .and_then(|json| {
                GoogleChatTokenCache::new(&json)
                    .map_err(|e| warn!("googlechat SA key error: {e}"))
                    .ok()
            });
        let jwt_verifier = audience.map(|aud| {
            info!("googlechat webhook JWT verification enabled (audience={aud})");
            GoogleChatJwtVerifier::new(aud)
        });
        // Precedence at send time (see `get_token`): SA key > ADC > static token.
        if use_adc {
            if key_configured && token_cache.is_none() {
                // A key WAS configured but failed to load. Don't switch identity
                // silently: name it. get_token still falls to ADC, but the operator
                // is told the bot now presents the workload identity, not the key's.
                warn!(
                    "Google Chat SA key was configured but could not be loaded; \
                     falling back to the keyless ADC (workload) identity — this is NOT \
                     the configured key identity. Fix the key, or unset use_adc."
                );
            } else if token_cache.is_none() {
                info!("googlechat keyless ADC enabled (chat.bot via IAM Credentials)");
            }
        }
        let mut adapter = Self::new(token_cache, access_token, jwt_verifier);
        // Install ADC only when it can actually be consulted: a loaded SA key
        // wins outright at send time (see `get_token`), so don't construct a
        // dead-at-runtime source behind it. A configured key that FAILED to
        // load still installs ADC — that is the named fallback warned above.
        adapter.metadata_source =
            (use_adc && adapter.token_cache.is_none()).then(MetadataTokenSource::new);
        adapter
    }

    pub fn new(
        token_cache: Option<GoogleChatTokenCache>,
        access_token: Option<String>,
        jwt_verifier: Option<GoogleChatJwtVerifier>,
    ) -> Self {
        Self {
            token_cache,
            metadata_source: None,
            access_token,
            jwt_verifier,
            client: reqwest::Client::new(),
            api_base: GOOGLE_CHAT_API_BASE.into(),
        }
    }

    /// Resolve the outbound bearer token. Precedence: SA key (`token_cache`)
    /// > ADC (`metadata_source`) > static `access_token`.
    ///
    /// Failure behavior is asymmetric by design: an SA-key exchange error
    /// hard-fails (`None` — the operator explicitly configured that identity,
    /// so nothing is silently substituted), while an ADC mint error falls
    /// through to the static token (both are the same workload's credential,
    /// so degrading is not an identity switch).
    async fn get_token(&self) -> Option<String> {
        if let Some(ref cache) = self.token_cache {
            match cache.get_token(&self.client).await {
                Ok(t) => return Some(t),
                Err(e) => {
                    error!("googlechat token refresh failed: {e}");
                    return None;
                }
            }
        }
        if let Some(ref src) = self.metadata_source {
            match src.get_token().await {
                Ok(t) => return Some(t),
                Err(e) => {
                    // Fall through to a configured static token instead of
                    // dropping the reply. ADC and the static token are the same
                    // workload's own credential, so this is not an identity switch.
                    if self.access_token.is_some() {
                        error!(
                            "googlechat ADC token mint failed ({e}); \
                             falling back to the configured static access_token"
                        );
                    } else {
                        error!("googlechat ADC token mint failed: {e}");
                    }
                }
            }
        }
        self.access_token.clone()
    }

    async fn edit_message(&self, message_name: &str, text: &str) {
        let Some(token) = self.get_token().await else {
            tracing::warn!("googlechat edit_message: no token available");
            return;
        };

        let formatted = markdown_to_gchat(text);
        let url = format!(
            "{}/{}?updateMask=text",
            self.api_base, message_name
        );
        let body = serde_json::json!({ "text": formatted });

        match self.client.patch(&url).bearer_auth(&token).json(&body).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::trace!(message_name = %message_name, "googlechat message edited");
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                error!(status = %status, body = %body, "googlechat edit_message failed");
            }
            Err(e) => {
                error!(err = %e, "googlechat edit_message request failed");
            }
        }
    }

    pub async fn handle_reply(
        &self,
        reply: &GatewayReply,
        event_tx: &tokio::sync::broadcast::Sender<String>,
    ) {
        // Command routing
        match reply.command.as_deref() {
            Some("add_reaction") | Some("remove_reaction") | Some("create_topic") => return,
            Some("edit_message") => {
                // Google Chat is send-once (see core's `NON_STREAMING_PLATFORMS`):
                // the unified adapter's synthetic `unified_<hex>` id is not a
                // valid `spaces/*/messages/*` resource name, and `patch` rejects
                // it with 400 INVALID_ARGUMENT before any edit applies. Refuse
                // non-resource-name ids here instead of sending a doomed request,
                // so a future caller cannot silently reintroduce that failure.
                if !reply.reply_to.starts_with("spaces/") {
                    tracing::warn!(
                        reply_to = %reply.reply_to,
                        "googlechat edit_message ignored: not a message resource name \
                         (synthetic ids cannot be patched)"
                    );
                    return;
                }
                self.edit_message(&reply.reply_to, &reply.content.text).await;
                return;
            }
            _ => {}
        }

        info!(
            space = %reply.channel.id,
            thread_id = ?reply.channel.thread_id,
            "gateway → googlechat"
        );

        let Some(token) = self.get_token().await else {
            info!(
                text = %reply.content.text,
                "googlechat reply (dry-run, no credentials configured)"
            );
            if let Some(ref req_id) = reply.request_id {
                let resp = crate::schema::GatewayResponse {
                    schema: "openab.gateway.response.v1".into(),
                    request_id: req_id.clone(),
                    success: false,
                    thread_id: None,
                    message_id: None,
                    error: Some("no credentials configured".into()),
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = event_tx.send(json);
                }
            }
            return;
        };

        let text = &reply.content.text;
        let chunks = split_text(text, GOOGLE_CHAT_MESSAGE_LIMIT);

        // Empty message: short-circuit, send failure ack and skip API call
        if chunks.is_empty() {
            if let Some(ref req_id) = reply.request_id {
                let resp = crate::schema::GatewayResponse {
                    schema: "openab.gateway.response.v1".into(),
                    request_id: req_id.clone(),
                    success: false,
                    thread_id: None,
                    message_id: None,
                    error: Some("empty message".into()),
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = event_tx.send(json);
                }
            }
            return;
        }

        if chunks.len() == 1 {
            let result = send_message(
                &self.client,
                &token,
                &reply.channel.id,
                reply.channel.thread_id.as_deref(),
                text,
                &self.api_base,
            )
            .await;

            if let Some(ref req_id) = reply.request_id {
                let (success, message_id, error) = match result {
                    Ok(name) => (true, Some(name), None),
                    Err(e) => (false, None, Some(e)),
                };
                let resp = crate::schema::GatewayResponse {
                    schema: "openab.gateway.response.v1".into(),
                    request_id: req_id.clone(),
                    success,
                    thread_id: None,
                    message_id,
                    error,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = event_tx.send(json);
                }
            }
        } else {
            let mut first_msg_name: Option<String> = None;
            let mut first_error: Option<String> = None;
            for chunk in chunks {
                match send_message(
                    &self.client,
                    &token,
                    &reply.channel.id,
                    reply.channel.thread_id.as_deref(),
                    chunk,
                    &self.api_base,
                )
                .await
                {
                    Ok(name) => {
                        if first_msg_name.is_none() {
                            first_msg_name = Some(name);
                        }
                    }
                    Err(e) => {
                        if first_error.is_none() {
                            first_error = Some(e);
                        }
                    }
                }
            }
            if let Some(ref req_id) = reply.request_id {
                let resp = crate::schema::GatewayResponse {
                    schema: "openab.gateway.response.v1".into(),
                    request_id: req_id.clone(),
                    success: first_msg_name.is_some() && first_error.is_none(),
                    thread_id: None,
                    message_id: first_msg_name,
                    error: first_error,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = event_tx.send(json);
                }
            }
        }
    }
}

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    info!("googlechat webhook received ({} bytes)", body.len());

    if let Some(ref adapter) = state.google_chat {
        if let Some(ref verifier) = adapter.jwt_verifier {
            let auth_header = match headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
            {
                Some(h) => h,
                None => {
                    warn!("googlechat webhook: missing authorization header");
                    return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
                }
            };
            if let Err(e) = verifier.verify(auth_header).await {
                warn!(error = %e, "googlechat webhook JWT verification failed");
                return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
            }
        }
    }

    let envelope: GoogleChatEnvelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            let body_str = String::from_utf8_lossy(&body);
            error!(body = %body_str, "googlechat webhook parse error: {e}");
            return (axum::http::StatusCode::BAD_REQUEST, "bad request").into_response();
        }
    };

    // Try the Pub/Sub `chat`-wrapped shape first, then fall back to the
    // HTTP endpoint URL top-level shape.
    let (msg_opt, top_user, top_space) = if let Some(chat) = envelope.chat {
        let user = chat.user;
        let (msg, space) = match chat.message_payload {
            Some(p) => (p.message, p.space),
            None => (None, None),
        };
        (msg, user, space)
    } else {
        (envelope.message, envelope.user, envelope.space)
    };

    let Some(ref msg) = msg_opt else {
        return empty_json_response();
    };

    let text = msg
        .argument_text
        .as_deref()
        .or(msg.text.as_deref())
        .unwrap_or("");

    let media_refs = parse_attachments(&msg.attachment);

    // Drop event only if BOTH text and attachments are empty
    if text.trim().is_empty() && media_refs.is_empty() {
        return empty_json_response();
    }

    let sender = msg.sender.as_ref().or(top_user.as_ref());
    let space = msg.space.as_ref().or(top_space.as_ref());

    let is_bot = sender.map(|s| s.user_type == "BOT").unwrap_or(false);
    if is_bot {
        return empty_json_response();
    }

    let sender_id = sender.map(|s| s.name.clone()).unwrap_or_default();
    let display_name = sender
        .map(|s| s.display_name.clone())
        .unwrap_or_else(|| "Unknown".into());
    let sender_name = sender_id
        .strip_prefix("users/")
        .unwrap_or(&sender_id)
        .to_string();

    let space_name = space.map(|s| s.name.clone()).unwrap_or_default();
    let space_type = space
        .and_then(|s| s.space_type.clone())
        .unwrap_or_else(|| "ROOM".into());

    let thread_id = msg.thread.as_ref().map(|t| t.name.clone());

    let message_id = msg
        .name
        .rsplit('/')
        .next()
        .unwrap_or(&msg.name)
        .to_string();

    // No attachments → emit event synchronously and respond 200
    if media_refs.is_empty() {
        send_googlechat_event(
            &state,
            &space_name,
            space_type,
            thread_id,
            &sender_id,
            &sender_name,
            &display_name,
            text,
            &message_id,
            Vec::new(),
        );
        return empty_json_response();
    }

    // Has attachments — spawn background task so the webhook returns 200 within
    // Google Chat's 30 s deadline regardless of how long downloads take.
    let text = text.to_string();
    let state = state.clone();
    let spawn_space = space_name.clone();
    tokio::spawn(async move {
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(async {
        let mut downloaded: Vec<crate::schema::Attachment> = Vec::new();
        let mut text_file_count: usize = 0;
        let mut text_file_bytes: u64 = 0;
        if let Some(ref adapter) = state.google_chat {
            if let Some(token) = adapter.get_token().await {
                for media_ref in &media_refs {
                    let attachment = match media_ref {
                        GoogleChatMediaRef::Image {
                            resource_name,
                            content_name,
                            ..
                        } => {
                            download_googlechat_image(
                                &adapter.client,
                                &token,
                                &adapter.api_base,
                                resource_name,
                                content_name,
                            )
                            .await
                        }
                        GoogleChatMediaRef::File {
                            resource_name,
                            content_name,
                            ..
                        } => {
                            if text_file_count >= TEXT_FILE_COUNT_CAP {
                                warn!(content_name = %content_name, cap = TEXT_FILE_COUNT_CAP, "googlechat text file count cap reached, skipping");
                                continue;
                            }
                            let remaining = TEXT_TOTAL_CAP.saturating_sub(text_file_bytes);
                            let att = download_googlechat_file(
                                &adapter.client,
                                &token,
                                &adapter.api_base,
                                resource_name,
                                content_name,
                                remaining,
                            )
                            .await;
                            if att.status.is_none() {
                                text_file_count += 1;
                                text_file_bytes += att.size;
                            }
                            att
                        }
                        GoogleChatMediaRef::Audio {
                            resource_name,
                            content_name,
                            content_type,
                        } => {
                            download_googlechat_audio(
                                &adapter.client,
                                &token,
                                &adapter.api_base,
                                resource_name,
                                content_name,
                                content_type,
                            )
                            .await
                        }
                    };
                    downloaded.push(attachment);
                }
            } else {
                warn!("googlechat: no token available for attachment download");
            }
        }

        // If text is empty AND every attachment failed to download, drop the event.
        if text.trim().is_empty() && downloaded.is_empty() {
            warn!(
                space = %space_name,
                "googlechat: empty text + all attachments failed, dropping event"
            );
            return;
        }

        send_googlechat_event(
            &state,
            &space_name,
            space_type,
            thread_id,
            &sender_id,
            &sender_name,
            &display_name,
            &text,
            &message_id,
            downloaded,
        );
        }).catch_unwind().await;
        if let Err(e) = result {
            error!(space = %spawn_space, "googlechat attachment download task panicked: {e:?}");
        }
    });

    empty_json_response()
}

#[allow(clippy::too_many_arguments)]
fn send_googlechat_event(
    state: &Arc<crate::AppState>,
    space_name: &str,
    space_type: String,
    thread_id: Option<String>,
    sender_id: &str,
    sender_name: &str,
    display_name: &str,
    text: &str,
    message_id: &str,
    attachments: Vec<crate::schema::Attachment>,
) {
    let mut gw_event = GatewayEvent::new(
        "googlechat",
        ChannelInfo {
            id: space_name.to_string(),
            channel_type: space_type,
            thread_id,
        },
        SenderInfo {
            id: sender_id.to_string(),
            name: sender_name.to_string(),
            display_name: display_name.to_string(),
            is_bot: false,
        },
        text,
        message_id,
        vec![],
    );
    gw_event.content.attachments = attachments;

    let attachment_count = gw_event.content.attachments.len();
    let json = match serde_json::to_string(&gw_event) {
        Ok(j) => j,
        Err(e) => {
            error!(error = %e, "googlechat: failed to serialize GatewayEvent");
            return;
        }
    };
    info!(
        space = %space_name,
        sender = %sender_name,
        attachment_count,
        "googlechat → gateway"
    );
    let _ = state.event_tx.send(json);
}

fn empty_json_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        "{}",
    )
        .into_response()
}

// --- Token cache with JWT auto-refresh ---

pub struct GoogleChatTokenCache {
    token: RwLock<Option<(String, Instant, u64)>>,
    sa_email: String,
    private_key: String,
}

const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

impl GoogleChatTokenCache {
    pub fn new(sa_key_json: &str) -> Result<Self, String> {
        let key: serde_json::Value =
            serde_json::from_str(sa_key_json).map_err(|e| format!("invalid SA key JSON: {e}"))?;
        let email = key
            .get("client_email")
            .and_then(|v| v.as_str())
            .ok_or("missing client_email in SA key")?
            .to_string();
        let pkey = key
            .get("private_key")
            .and_then(|v| v.as_str())
            .ok_or("missing private_key in SA key")?
            .to_string();
        Ok(Self {
            token: RwLock::new(None),
            sa_email: email,
            private_key: pkey,
        })
    }

    pub async fn get_token(&self, client: &reqwest::Client) -> Result<String, String> {
        {
            let guard = self.token.read().await;
            if let Some((ref tok, ref ts, ttl)) = *guard {
                if ts.elapsed().as_secs() < refresh_threshold(ttl) {
                    return Ok(tok.clone());
                }
            }
        }
        let mut guard = self.token.write().await;
        if let Some((ref tok, ref ts, ttl)) = *guard {
            if ts.elapsed().as_secs() < refresh_threshold(ttl) {
                return Ok(tok.clone());
            }
        }
        match self.refresh(client).await {
            Ok((new_token, expire)) => {
                *guard = Some((new_token.clone(), Instant::now(), expire));
                info!("googlechat access token refreshed (expires in {expire}s)");
                Ok(new_token)
            }
            Err(e) => {
                // Serve the still-valid cached token on a transient exchange
                // failure instead of dropping the reply.
                if let Some((ref tok, ref ts, ttl)) = *guard {
                    let elapsed = ts.elapsed().as_secs();
                    if elapsed < ttl {
                        warn!(
                            "googlechat token refresh failed ({e}); serving cached token \
                             still valid for {}s",
                            ttl - elapsed
                        );
                        return Ok(tok.clone());
                    }
                }
                Err(e)
            }
        }
    }

    async fn refresh(&self, client: &reqwest::Client) -> Result<(String, u64), String> {
        let jwt = self.build_jwt().map_err(|e| format!("JWT build error: {e}"))?;
        let resp = client
            .post("https://oauth2.googleapis.com/token")
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| format!("token exchange request failed: {e}"))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("token exchange parse failed: {e}"))?;

        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            // Boundary validation: an empty token would be cached as "valid"
            // and fail every send with 401 until the refresh threshold.
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                let err = body
                    .get("error_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                format!("token exchange returned missing/empty access_token: {err}")
            })?
            .to_string();

        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);

        Ok((token, expires_in))
    }

    fn build_jwt(&self) -> Result<String, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();

        let claims = serde_json::json!({
            "iss": self.sa_email,
            "scope": "https://www.googleapis.com/auth/chat.bot",
            "aud": "https://oauth2.googleapis.com/token",
            "iat": now,
            "exp": now + 3600,
        });

        let key = jsonwebtoken::EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .map_err(|e| format!("RSA key parse error: {e}"))?;
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        jsonwebtoken::encode(&header, &claims, &key)
            .map_err(|e| format!("JWT encode error: {e}"))
    }
}

// --- Keyless ADC token source (GCE metadata → IAM Credentials) ---

/// Google Chat's `chat.bot` scope for the minted token.
const ADC_CHAT_BOT_SCOPE: &str = "https://www.googleapis.com/auth/chat.bot";
/// Lifetime we request from IAM Credentials for the impersonated token, and
/// the fallback TTL we cache it under. IAM caps impersonated tokens at 3600s.
const ADC_TOKEN_LIFETIME_SECS: u64 = 3600;

/// Cache TTL (seconds) to use for a minted token, derived from the IAM
/// response's `expireTime`. Falls back to the full lifetime when the field is
/// missing/unparseable, and clamps to `[0, ADC_TOKEN_LIFETIME_SECS]` so an
/// org-policy-shortened token isn't cached past its real expiry (0 forces a
/// fresh mint next call rather than serving a dead token).
fn ttl_from_expire_time(expire_time: &str, now: chrono::DateTime<chrono::Utc>) -> u64 {
    match chrono::DateTime::parse_from_rfc3339(expire_time) {
        Ok(exp) => (exp.with_timezone(&chrono::Utc) - now)
            .num_seconds()
            .clamp(0, ADC_TOKEN_LIFETIME_SECS as i64) as u64,
        Err(_) => ADC_TOKEN_LIFETIME_SECS,
    }
}

/// Age (seconds) at which a cached token must be refreshed: its ttl minus a
/// margin, where the margin is capped at half the ttl. A fixed 300 s margin
/// would make `elapsed < ttl - 300` always false for any `ttl <= 300`, forcing
/// a re-mint on every single send; capping keeps short-lived tokens cacheable.
/// For `ttl = 0` the threshold is 0, so an expired token is never served from
/// the cache.
fn refresh_threshold(ttl: u64) -> u64 {
    ttl.saturating_sub(TOKEN_REFRESH_MARGIN_SECS.min(ttl / 2))
}

/// Mints a `chat.bot`-scoped access token **without** a service-account key
/// file, using the workload's own identity (keyless ADC). Flow, per refresh:
///   1. read the default SA's email + a base token from the GCE metadata server
///   2. call IAM Credentials `generateAccessToken` (self-impersonation) to
///      exchange the base token for a `chat.bot`-scoped token
///
/// Requires `roles/iam.serviceAccountTokenCreator` on the SA over itself and
/// the `iamcredentials.googleapis.com` API enabled. The `*_base` fields are
/// overridable so tests can point them at a mock server.
pub struct MetadataTokenSource {
    token: RwLock<Option<(String, Instant, u64)>>,
    // Private: only `new` (prod, fixed trusted hosts) or the in-module
    // `with_bases` (tests, mock server) may set these. Unexported ⇒ no in-process
    // caller can retarget the metadata bearer to an arbitrary host.
    metadata_base: String,
    iam_credentials_base: String,
    // No-redirect client: a redirect from either endpoint must never carry
    // the metadata bearer (`Authorization`) on to a third host.
    client: reqwest::Client,
}

impl Default for MetadataTokenSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataTokenSource {
    /// Production constructor: the fixed, trusted GCP endpoints (IAM over HTTPS).
    pub fn new() -> Self {
        Self::with_bases(
            "http://metadata.google.internal".into(),
            "https://iamcredentials.googleapis.com".into(),
        )
    }

    /// Construct with explicit endpoint bases. Prod always goes through `new`
    /// with HTTPS IAM; tests point these at a mock server.
    fn with_bases(metadata_base: String, iam_credentials_base: String) -> Self {
        Self {
            token: RwLock::new(None),
            metadata_base,
            iam_credentials_base,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        }
    }

    /// Cached-or-refresh, mirroring [`GoogleChatTokenCache::get_token`]:
    /// double-checked locking around the RwLock so only one refresh runs.
    pub async fn get_token(&self) -> Result<String, String> {
        {
            let guard = self.token.read().await;
            if let Some((ref tok, ref ts, ttl)) = *guard {
                if ts.elapsed().as_secs() < refresh_threshold(ttl) {
                    return Ok(tok.clone());
                }
            }
        }
        let mut guard = self.token.write().await;
        if let Some((ref tok, ref ts, ttl)) = *guard {
            if ts.elapsed().as_secs() < refresh_threshold(ttl) {
                return Ok(tok.clone());
            }
        }
        match self.refresh().await {
            Ok(minted) => {
                let MintedToken { token, ttl, sa_email } = minted;
                if ttl == 0 {
                    // Freshly minted but already at/after its expireTime — almost
                    // always local clock skew (Google validates against its own
                    // clock, so the token may still work). Serve it once, but do
                    // not cache a dead token: the next call re-mints.
                    warn!(
                        service_account = %sa_email,
                        "googlechat ADC minted a token with ttl=0 (clock skew?); \
                         serving once without caching"
                    );
                    return Ok(token);
                }
                *guard = Some((token.clone(), Instant::now(), ttl));
                // Name the resolved identity so on-call can confirm which SA
                // self-impersonation actually used when diagnosing send errors.
                info!(
                    service_account = %sa_email,
                    "googlechat ADC token minted (chat.bot, ttl {ttl}s)"
                );
                Ok(token)
            }
            Err(e) => {
                // During a transient metadata/IAM failure, serve the cached
                // token while it is still valid rather than dropping the reply.
                if let Some((ref tok, ref ts, ttl)) = *guard {
                    let elapsed = ts.elapsed().as_secs();
                    if elapsed < ttl {
                        warn!(
                            "googlechat ADC refresh failed ({e}); serving cached token \
                             still valid for {}s",
                            ttl - elapsed
                        );
                        return Ok(tok.clone());
                    }
                }
                Err(e)
            }
        }
    }

    async fn refresh(&self) -> Result<MintedToken, String> {
        // Use the source's own no-redirect client for every bearer-carrying call.
        let client = &self.client;
        // 1. Default SA email from the GCE metadata server.
        let email = client
            .get(format!(
                "{}/computeMetadata/v1/instance/service-accounts/default/email",
                self.metadata_base
            ))
            .header("Metadata-Flavor", "Google")
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("metadata email request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("metadata email status: {e}"))?
            .text()
            .await
            .map_err(|e| format!("metadata email read failed: {e}"))?;
        let email = email.trim();
        if email.is_empty() {
            return Err("metadata returned empty SA email".into());
        }

        // 2. Base access token for the default SA from the metadata server.
        let base: serde_json::Value = client
            .get(format!(
                "{}/computeMetadata/v1/instance/service-accounts/default/token",
                self.metadata_base
            ))
            .header("Metadata-Flavor", "Google")
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("metadata token request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("metadata token status: {e}"))?
            .json()
            .await
            .map_err(|e| format!("metadata token parse failed: {e}"))?;
        let base_token = base
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or("metadata token response missing or empty access_token")?;

        // 3. Exchange the base token for a chat.bot-scoped token via IAM
        //    Credentials generateAccessToken (the SA impersonates itself).
        let url = format!(
            "{}/v1/projects/-/serviceAccounts/{email}:generateAccessToken",
            self.iam_credentials_base
        );
        let resp = client
            .post(&url)
            .bearer_auth(base_token)
            .json(&serde_json::json!({
                "scope": [ADC_CHAT_BOT_SCOPE],
                "lifetime": format!("{ADC_TOKEN_LIFETIME_SECS}s"),
            }))
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("generateAccessToken request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            // Classify the common GCP causes so on-call can act on the log
            // line directly instead of decoding GCP error prose (mirrors the
            // operator guidance in docs/google-chat.md Option C).
            let body: String = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(400)
                .collect();
            let reason = classify_generate_access_token_error(status.as_u16(), &body);
            return Err(format!(
                "generateAccessToken failed (status {status}, reason={reason}): {body}"
            ));
        }
        let resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("generateAccessToken parse failed: {e}"))?;
        let token = resp
            .get("accessToken")
            .and_then(|v| v.as_str())
            // Boundary validation: an empty token would be cached as "valid"
            // for its full TTL and bypass the static-token degradation path.
            .filter(|s| !s.trim().is_empty())
            .ok_or("generateAccessToken response missing or empty accessToken")?
            .to_string();
        // Cache under the server-granted lifetime (respects an org policy that
        // shortens impersonated tokens below the requested 3600s), falling back
        // to the full lifetime when expireTime is absent/unparseable.
        let ttl = resp
            .get("expireTime")
            .and_then(|v| v.as_str())
            .map(|e| ttl_from_expire_time(e, chrono::Utc::now()))
            .unwrap_or(ADC_TOKEN_LIFETIME_SECS);
        Ok(MintedToken {
            token,
            ttl,
            sa_email: email.to_string(),
        })
    }
}

/// A successfully minted ADC token plus the identity that minted it, so the
/// caller can log which service account self-impersonation resolved to.
struct MintedToken {
    token: String,
    ttl: u64,
    sa_email: String,
}

/// Best-effort classification of common GCP `generateAccessToken` failures.
/// Ordering matters: the insufficient-scope 403 body says "scopes" (not
/// "permission"), which is the documented signal distinguishing it from a
/// missing `roles/iam.serviceAccountTokenCreator` binding.
fn classify_generate_access_token_error(status: u16, body: &str) -> &'static str {
    let b = body.to_ascii_lowercase();
    if b.contains("api has not been used") || b.contains("service_disabled") || b.contains("is disabled")
    {
        "api_not_enabled"
    } else if status == 403 && b.contains("scopes") {
        "insufficient_scope"
    } else if status == 403 {
        "missing_role"
    } else {
        "unclassified"
    }
}

/// Convert markdown to Google Chat native formatting.
///
/// Called by both `send_message` and `edit_message`. Assumes the caller passes
/// **raw markdown** — passing already-converted text would double-convert
/// (e.g. `*bold*` from a previous pass would be re-parsed as `*italic*`).
/// OAB core is expected to always emit raw markdown for both initial replies
/// and streaming edits.
fn markdown_to_gchat(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Detect fenced code block — pass through unchanged
        if line.trim_start().starts_with("```") {
            result.push_str(line);
            result.push('\n');
            i += 1;
            while i < lines.len() {
                result.push_str(lines[i]);
                if lines[i].trim_start().starts_with("```") {
                    i += 1;
                    if i < lines.len() {
                        result.push('\n');
                    }
                    break;
                }
                result.push('\n');
                i += 1;
            }
            continue;
        }
        // Heading → bold
        let converted = if let Some(heading) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            format!("*{}*", heading.trim())
        } else {
            convert_inline(line)
        };
        result.push_str(&converted);
        i += 1;
        if i < lines.len() {
            result.push('\n');
        }
    }
    result
}

// TODO(perf): allocates Vec<char> per line. Acceptable at current scale,
// but on hot streaming paths with many edit_message updates this could be
// rewritten with byte-level iteration over &str.
fn convert_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Inline code — pass through
        if chars[i] == '`' {
            out.push('`');
            i += 1;
            while i < chars.len() && chars[i] != '`' {
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push('`');
                i += 1;
            }
            continue;
        }
        // Markdown link: [text](url)
        if chars[i] == '[' {
            if let Some((link_text, url, end)) = parse_md_link(&chars, i) {
                let converted_text = convert_inline(&link_text);
                out.push_str(&format!("<{}|{}>", url, converted_text));
                i = end;
                continue;
            }
        }
        // Bold: **text** → *text*
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_closing(&chars, i + 2, &['*', '*']) {
                out.push('*');
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&convert_inline(&inner));
                out.push('*');
                i = end + 2;
                continue;
            }
        }
        // Bold: __text__ → *text*
        if chars[i] == '_' && i + 1 < chars.len() && chars[i + 1] == '_' {
            if let Some(end) = find_closing(&chars, i + 2, &['_', '_']) {
                out.push('*');
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&convert_inline(&inner));
                out.push('*');
                i = end + 2;
                continue;
            }
        }
        // Strikethrough: ~~text~~ → ~text~
        if chars[i] == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some(end) = find_closing(&chars, i + 2, &['~', '~']) {
                out.push('~');
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&convert_inline(&inner));
                out.push('~');
                i = end + 2;
                continue;
            }
        }
        // Italic: *text* → _text_ (single asterisk, not part of **bold**)
        // Must come AFTER the **bold** check above. Requires non-asterisk
        // immediately after opening * and before closing *.
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] != '*'
            && !chars[i + 1].is_whitespace()
        {
            if let Some(end) = find_single(&chars, i + 1, '*') {
                if end > i + 1 && !chars[end - 1].is_whitespace() {
                    out.push('_');
                    let inner: String = chars[i + 1..end].iter().collect();
                    out.push_str(&convert_inline(&inner));
                    out.push('_');
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_single(chars: &[char], start: usize, target: char) -> Option<usize> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_md_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut i = start + 1;
    let mut depth = 1;
    let text_start = i;
    while i < chars.len() && depth > 0 {
        if chars[i] == '[' {
            depth += 1;
        } else if chars[i] == ']' {
            depth -= 1;
        }
        if depth > 0 {
            i += 1;
        }
    }
    if depth != 0 {
        return None;
    }
    let text: String = chars[text_start..i].iter().collect();
    i += 1; // skip ']'
    if i >= chars.len() || chars[i] != '(' {
        return None;
    }
    i += 1; // skip '('
    let url_start = i;
    let mut paren_depth = 1;
    while i < chars.len() && paren_depth > 0 {
        if chars[i] == '(' {
            paren_depth += 1;
        } else if chars[i] == ')' {
            paren_depth -= 1;
        }
        if paren_depth > 0 {
            i += 1;
        }
    }
    if paren_depth != 0 {
        return None;
    }
    let url: String = chars[url_start..i].iter().collect();
    Some((text, url, i + 1))
}

fn find_closing(chars: &[char], start: usize, pattern: &[char]) -> Option<usize> {
    if pattern.len() < 2 {
        return None;
    }
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == pattern[0] && chars[i + 1] == pattern[1] {
            return Some(i);
        }
        i += 1;
    }
    None
}

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    space: &str,
    thread_id: Option<&str>,
    text: &str,
    api_base: &str,
) -> Result<String, String> {
    let mut url = format!("{}/{}/messages", api_base, space);

    let formatted = markdown_to_gchat(text);
    let mut body = serde_json::json!({
        "text": formatted,
    });

    if let Some(thread_id) = thread_id {
        body["thread"] = serde_json::json!({
            "name": thread_id,
        });
        url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
    }

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.text().await.unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            parsed
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or_else(|| "missing message name in response".into())
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "googlechat send error");
            Err(format!("send failed: {} {}", status, body))
        }
        Err(e) => {
            error!("googlechat send error: {e}");
            Err(format!("request error: {e}"))
        }
    }
}

fn split_text(text: &str, limit: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        if start + limit >= text.len() {
            chunks.push(&text[start..]);
            break;
        }
        let mut end = start + limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut search_start = if end > start + 200 { end - 200 } else { start };
        while search_start < end && !text.is_char_boundary(search_start) {
            search_start += 1;
        }
        let break_at = text[search_start..end]
            .rfind('\n')
            .or_else(|| text[search_start..end].rfind(' '))
            .map(|pos| search_start + pos + 1)
            .unwrap_or(end);
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

// --- Attachment parsing & download ---

/// Whitelist of text-like file extensions for `download_googlechat_file`.
const TEXT_EXTS: &[&str] = &[
    "txt", "csv", "log", "md", "json", "jsonl", "yaml", "yml", "toml", "xml",
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp",
    "rb", "sh", "bash", "sql", "html", "css", "ini", "cfg", "conf",
];

/// Parse Google Chat attachment array into media references for async download.
///
/// Skips Drive-sourced attachments (different download API), and unknown
/// content types. Branches on `contentType` prefix to bucket into image /
/// audio / file.
fn parse_attachments(attachments: &[GoogleChatAttachment]) -> Vec<GoogleChatMediaRef> {
    let mut refs = Vec::new();
    for att in attachments {
        // Only handle UPLOADED_CONTENT (Drive needs separate Drive API call)
        if att.source.as_deref() != Some("UPLOADED_CONTENT") {
            continue;
        }
        let resource_name = match att
            .attachment_data_ref
            .as_ref()
            .and_then(|d| d.resource_name.clone())
        {
            Some(rn) => rn,
            None => continue,
        };
        let content_type = att.content_type.clone().unwrap_or_default();
        let content_name = att.content_name.clone().unwrap_or_else(|| "file".into());

        if content_type.starts_with("image/") {
            refs.push(GoogleChatMediaRef::Image {
                resource_name,
                content_name,
            });
        } else if content_type.starts_with("audio/") {
            refs.push(GoogleChatMediaRef::Audio {
                resource_name,
                content_name,
                content_type,
            });
        } else if content_type.starts_with("video/") {
            info!(content_name = %content_name, content_type = %content_type, "googlechat: video attachment skipped (not yet supported)");
        } else {
            refs.push(GoogleChatMediaRef::File {
                resource_name,
                content_name,
            });
        }
    }
    refs
}

/// Resize image so longest side ≤ 1200px, then encode as JPEG.
/// GIFs are passed through unchanged to preserve animation.
fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    use image::ImageReader;
    use std::io::Cursor;

    let reader = ImageReader::new(Cursor::new(raw)).with_guessed_format()?;
    let format = reader.format();
    if format == Some(image::ImageFormat::Gif) {
        return Ok((raw.to_vec(), "image/gif".to_string()));
    }
    let img = reader.decode()?;
    let (w, h) = (img.width(), img.height());
    let img = if w > IMAGE_MAX_DIMENSION_PX || h > IMAGE_MAX_DIMENSION_PX {
        let max_side = std::cmp::max(w, h);
        let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::from(max_side);
        let new_w = (f64::from(w) * ratio) as u32;
        let new_h = (f64::from(h) * ratio) as u32;
        img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, IMAGE_JPEG_QUALITY);
    img.write_with_encoder(encoder)?;
    Ok((buf.into_inner(), "image/jpeg".to_string()))
}

/// Build the Media API URL for a given resource_name.
/// Google Chat Media API uses `{+resourceName}` (RFC 6570 reserved expansion),
/// so `/` must stay literal while other special chars are percent-encoded.
fn media_url(api_base: &str, resource_name: &str) -> String {
    let encoded: String = resource_name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect();
    format!("{}/media/{}?alt=media", api_base, encoded)
}

/// Download an image attachment via Google Chat Media API → resize/compress → base64.
pub async fn download_googlechat_image(
    client: &reqwest::Client,
    token: &str,
    api_base: &str,
    resource_name: &str,
    content_name: &str,
) -> crate::schema::Attachment {
    let url = media_url(api_base, resource_name);
    let resp = match client.get(&url).bearer_auth(token).timeout(MEDIA_REQUEST_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat image download failed");
            return crate::schema::Attachment::rejected(
                "image",
                content_name.to_string(),
                "image/jpeg",
                0,
                "download failed: network error",
            );
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        warn!(content_name, status = %status, "googlechat image download failed");
        return crate::schema::Attachment::rejected(
            "image",
            content_name.to_string(),
            "image/jpeg",
            0,
            format!("download failed: HTTP {}", status.as_u16()),
        );
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > IMAGE_MAX_DOWNLOAD {
                warn!(content_name, size, "googlechat image Content-Length exceeds 10MB limit");
                return crate::schema::Attachment::rejected(
                    "image",
                    content_name.to_string(),
                    "image/jpeg",
                    size,
                    format!("size exceeded: {} exceeds {}", format_bytes(size), format_bytes(IMAGE_MAX_DOWNLOAD)),
                );
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat image body read failed");
            return crate::schema::Attachment::rejected(
                "image",
                content_name.to_string(),
                "image/jpeg",
                0,
                "download failed: body read error",
            );
        }
    };
    if bytes.len() as u64 > IMAGE_MAX_DOWNLOAD {
        warn!(content_name, size = bytes.len(), "googlechat image exceeds 10MB limit");
        return crate::schema::Attachment::rejected(
            "image",
            content_name.to_string(),
            "image/jpeg",
            bytes.len() as u64,
            format!("size exceeded: {} exceeds {}", format_bytes(bytes.len() as u64), format_bytes(IMAGE_MAX_DOWNLOAD)),
        );
    }
    let (compressed, mime) = match resize_and_compress(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat image resize failed");
            return crate::schema::Attachment::rejected(
                "image",
                content_name.to_string(),
                "image/jpeg",
                bytes.len() as u64,
                "processing failed: image encoding error",
            );
        }
    };
    let path = match crate::store::store_media(&compressed).await {
        Some(p) => p,
        None => {
            warn!(content_name, "googlechat image store failed");
            return crate::schema::Attachment::rejected(
                "image",
                content_name.to_string(),
                "image/jpeg",
                compressed.len() as u64,
                "processing failed: storage error",
            );
        }
    };
    crate::schema::Attachment {
        attachment_type: "image".into(),
        filename: content_name.to_string(),
        mime_type: mime,
        data: String::new(),
        size: compressed.len() as u64,
        path: Some(path),
        status: None,
    }
}

/// Download a text-like file via Google Chat Media API → base64.
/// Non-text extensions are skipped to avoid sending binary garbage to the model.
pub async fn download_googlechat_file(
    client: &reqwest::Client,
    token: &str,
    api_base: &str,
    resource_name: &str,
    content_name: &str,
    remaining_budget: u64,
) -> crate::schema::Attachment {
    let ext = content_name.rsplit('.').next().unwrap_or("").to_lowercase();
    if !TEXT_EXTS.contains(&ext.as_str()) {
        tracing::debug!(content_name, "skipping non-text googlechat file attachment");
        return crate::schema::Attachment::rejected(
            "text_file",
            content_name.to_string(),
            "text/plain",
            0,
            format!("unsupported format: {}", ext),
        );
    }
    let max_size = FILE_MAX_DOWNLOAD.min(remaining_budget);
    let url = media_url(api_base, resource_name);
    let resp = match client.get(&url).bearer_auth(token).timeout(MEDIA_REQUEST_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat file download failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                content_name.to_string(),
                "text/plain",
                0,
                "download failed: network error",
            );
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        warn!(content_name, status = %status, "googlechat file download failed");
        return crate::schema::Attachment::rejected(
            "text_file",
            content_name.to_string(),
            "text/plain",
            0,
            format!("download failed: HTTP {}", status.as_u16()),
        );
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > max_size {
                warn!(content_name, size, limit = max_size, "googlechat file Content-Length exceeds limit");
                return crate::schema::Attachment::rejected(
                    "text_file",
                    content_name.to_string(),
                    "text/plain",
                    size,
                    format!("size exceeded: {} exceeds {}", format_bytes(size), format_bytes(max_size)),
                );
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat file body read failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                content_name.to_string(),
                "text/plain",
                0,
                "download failed: body read error",
            );
        }
    };
    if bytes.len() as u64 > max_size {
        warn!(content_name, size = bytes.len(), limit = max_size, "googlechat file exceeds size limit");
        return crate::schema::Attachment::rejected(
            "text_file",
            content_name.to_string(),
            "text/plain",
            bytes.len() as u64,
            format!("size exceeded: {} exceeds {}", format_bytes(bytes.len() as u64), format_bytes(max_size)),
        );
    }
    let path = match crate::store::store_media(&bytes).await {
        Some(p) => p,
        None => {
            warn!(content_name, "googlechat file store failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                content_name.to_string(),
                "text/plain",
                bytes.len() as u64,
                "processing failed: storage error",
            );
        }
    };
    crate::schema::Attachment {
        attachment_type: "text_file".into(),
        filename: content_name.to_string(),
        mime_type: "text/plain".into(),
        data: String::new(),
        size: bytes.len() as u64,
        path: Some(path),
        status: None,
    }
}

/// Download an audio attachment as-is (no resize/transcode) → filesystem store.
/// Core's STT pipeline (when available) consumes this as `audio` attachment_type.
pub async fn download_googlechat_audio(
    client: &reqwest::Client,
    token: &str,
    api_base: &str,
    resource_name: &str,
    content_name: &str,
    content_type: &str,
) -> crate::schema::Attachment {
    let url = media_url(api_base, resource_name);
    let resp = match client.get(&url).bearer_auth(token).timeout(MEDIA_REQUEST_TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat audio download failed");
            return crate::schema::Attachment::rejected(
                "audio",
                content_name.to_string(),
                "audio/ogg",
                0,
                "download failed: network error",
            );
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        warn!(content_name, status = %status, "googlechat audio download failed");
        return crate::schema::Attachment::rejected(
            "audio",
            content_name.to_string(),
            "audio/ogg",
            0,
            format!("download failed: HTTP {}", status.as_u16()),
        );
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > AUDIO_MAX_DOWNLOAD {
                warn!(content_name, size, "googlechat audio Content-Length exceeds 25MB limit");
                return crate::schema::Attachment::rejected(
                    "audio",
                    content_name.to_string(),
                    "audio/ogg",
                    size,
                    format!("size exceeded: {} exceeds {}", format_bytes(size), format_bytes(AUDIO_MAX_DOWNLOAD)),
                );
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(content_name, error = %e, "googlechat audio body read failed");
            return crate::schema::Attachment::rejected(
                "audio",
                content_name.to_string(),
                "audio/ogg",
                0,
                "download failed: body read error",
            );
        }
    };
    if bytes.len() as u64 > AUDIO_MAX_DOWNLOAD {
        warn!(content_name, size = bytes.len(), "googlechat audio exceeds 25MB limit");
        return crate::schema::Attachment::rejected(
            "audio",
            content_name.to_string(),
            "audio/ogg",
            bytes.len() as u64,
            format!("size exceeded: {} exceeds {}", format_bytes(bytes.len() as u64), format_bytes(AUDIO_MAX_DOWNLOAD)),
        );
    }
    let path = match crate::store::store_media(&bytes).await {
        Some(p) => p,
        None => {
            warn!(content_name, "googlechat audio store failed");
            return crate::schema::Attachment::rejected(
                "audio",
                content_name.to_string(),
                "audio/ogg",
                bytes.len() as u64,
                "processing failed: storage error",
            );
        }
    };
    crate::schema::Attachment {
        attachment_type: "audio".into(),
        filename: content_name.to_string(),
        mime_type: content_type.to_string(),
        data: String::new(),
        size: bytes.len() as u64,
        path: Some(path),
        status: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Webhook parsing tests ---

    fn make_envelope(
        text: &str,
        argument_text: Option<&str>,
        sender_type: &str,
        space_type: &str,
        thread_name: Option<&str>,
    ) -> String {
        let arg_field = argument_text
            .map(|a| format!(r#""argumentText": "{a}","#))
            .unwrap_or_default();
        let thread_field = thread_name
            .map(|t| format!(r#","thread": {{"name": "{t}"}}"#))
            .unwrap_or_default();
        format!(
            r#"{{
                "chat": {{
                    "user": {{
                        "name": "users/111",
                        "displayName": "Test",
                        "type": "{sender_type}"
                    }},
                    "messagePayload": {{
                        "message": {{
                            "name": "spaces/SP/messages/msg1",
                            "text": "{text}",
                            {arg_field}
                            "sender": {{
                                "name": "users/111",
                                "displayName": "Test",
                                "type": "{sender_type}"
                            }},
                            "space": {{
                                "name": "spaces/SP",
                                "type": "{space_type}"
                            }}
                            {thread_field}
                        }},
                        "space": {{
                            "name": "spaces/SP",
                            "type": "{space_type}"
                        }}
                    }}
                }}
            }}"#
        )
    }

    #[test]
    fn parse_dm_message() {
        let json = make_envelope("hello", None, "HUMAN", "DM", None);
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let chat = envelope.chat.unwrap();
        let msg = chat.message_payload.unwrap().message.unwrap();
        assert_eq!(msg.text.as_deref(), Some("hello"));
        assert_eq!(msg.sender.unwrap().user_type, "HUMAN");
    }

    #[test]
    fn parse_space_message_with_thread() {
        let json = make_envelope(
            "@Bot hi",
            Some("hi"),
            "HUMAN",
            "ROOM",
            Some("spaces/SP/threads/t1"),
        );
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let chat = envelope.chat.unwrap();
        let payload = chat.message_payload.unwrap();
        let msg = payload.message.as_ref().unwrap();
        assert_eq!(msg.argument_text.as_deref(), Some("hi"));
        assert_eq!(msg.thread.as_ref().unwrap().name, "spaces/SP/threads/t1");
        assert_eq!(payload.space.as_ref().unwrap().space_type.as_deref(), Some("ROOM"));
    }

    #[test]
    fn parse_bot_message_detected() {
        let json = make_envelope("bot says hi", None, "BOT", "DM", None);
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let chat = envelope.chat.unwrap();
        let user = chat.user.unwrap();
        assert_eq!(user.user_type, "BOT");
    }

    #[test]
    fn parse_missing_chat_field() {
        let json = r#"{"type": "ADDED_TO_SPACE"}"#;
        let envelope: GoogleChatEnvelope = serde_json::from_str(json).unwrap();
        assert!(envelope.chat.is_none());
    }

    #[test]
    fn parse_missing_message_payload() {
        let json = r#"{"chat": {"user": {"name": "u/1", "displayName": "X", "type": "HUMAN"}}}"#;
        let envelope: GoogleChatEnvelope = serde_json::from_str(json).unwrap();
        assert!(envelope.chat.unwrap().message_payload.is_none());
    }

    #[test]
    fn parse_invalid_json() {
        let result: Result<GoogleChatEnvelope, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn argument_text_preferred_over_text() {
        let json = make_envelope("@Bot explain", Some("explain"), "HUMAN", "ROOM", None);
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let msg = envelope
            .chat
            .unwrap()
            .message_payload
            .unwrap()
            .message
            .unwrap();
        let text = msg
            .argument_text
            .as_deref()
            .or(msg.text.as_deref())
            .unwrap();
        assert_eq!(text, "explain");
    }

    #[test]
    fn sender_name_strips_users_prefix() {
        let sender_id = "users/123456";
        let name = sender_id.strip_prefix("users/").unwrap_or(sender_id);
        assert_eq!(name, "123456");
    }

    #[test]
    fn message_id_extracts_last_segment() {
        let msg_name = "spaces/SP/messages/abc123";
        let id = msg_name.rsplit('/').next().unwrap_or(msg_name);
        assert_eq!(id, "abc123");
    }

    // --- split_text tests ---

    #[test]
    fn split_text_short() {
        let chunks = split_text("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_text_exact_limit() {
        let text = "a".repeat(100);
        let chunks = split_text(&text, 100);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn split_text_over_limit() {
        let text = "a".repeat(150);
        let chunks = split_text(&text, 100);
        assert_eq!(chunks.len(), 2);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_text_breaks_at_newline() {
        let text = format!("{}\n{}", "a".repeat(50), "b".repeat(50));
        let chunks = split_text(&text, 60);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
    }

    #[test]
    fn split_text_breaks_at_space() {
        let text = format!("{} {}", "a".repeat(50), "b".repeat(50));
        let chunks = split_text(&text, 60);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_text_chinese_utf8_safe() {
        let text = "你好世界測試谷歌聊天中文消息分割安全驗證完成";
        let chunks = split_text(text, 10);
        assert!(chunks.len() > 1);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_text_search_start_char_boundary() {
        let text: String = "谷歌".repeat(150); // 300 chars, 900 bytes
        let chunks = split_text(&text, 500);
        assert!(chunks.len() >= 2);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_text_empty() {
        let chunks = split_text("", 100);
        assert!(chunks.is_empty());
    }

    // --- Token cache tests ---

    #[test]
    fn token_cache_rejects_invalid_json() {
        let result = GoogleChatTokenCache::new("not json");
        assert!(result.is_err());
    }

    #[test]
    fn token_cache_rejects_missing_fields() {
        match GoogleChatTokenCache::new(r#"{"type": "service_account"}"#) {
            Err(e) => assert!(e.contains("client_email"), "unexpected error: {e}"),
            Ok(_) => panic!("expected error for missing client_email"),
        }
    }

    #[test]
    fn token_cache_accepts_valid_sa_key() {
        let key = r#"{
            "type": "service_account",
            "client_email": "test@test.iam.gserviceaccount.com",
            "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMIIBogIBAAJBALvRE+oCMiEhtfO5ufaVc9wGPUMgPGxmVFiMPC/NMxmCSiMGNO9h\nCOyByeF78QHp4gOW/lgVU8MJkv33hVMbOr0CAwEAAQJAD2k/cFR5MIkw1PFcm98K\n9MqYKGpJCmGBjFY0ek0FHoC14d/hpAGaoWMjNaAyjU/IbGv1fj8C5MfFRal0fV/L\nAQIhAP0T6FPJMm3O4bM18kMHnOP2+Y5kxMpVxCCjkVNH7D09AiEAvXEQJYwR+PFs\njDDhEm4VPmk+lKJoQlopj8TN5gQV8DECIBcXbU+LPWx4H+qRElhCB1B5a9mYmpY\nV6LFPnvSfHqNAiEAiNj5+A6E7WJ50il+5NG5yn7gXh8vNxdCYIw5qx6C2bECIBmW\nVGVRhSmNsmDMJFsGIdKJsnEXpizIVHtfpXsS4j9X\n-----END RSA PRIVATE KEY-----\n"
        }"#;
        let result = GoogleChatTokenCache::new(key);
        assert!(result.is_ok());
    }

    // --- Keyless ADC (MetadataTokenSource) tests ---

    #[test]
    fn ttl_from_expire_time_derives_and_clamps() {
        use chrono::{DateTime, Utc};
        let now: DateTime<Utc> = "2026-08-25T00:00:00Z".parse().unwrap();
        // Normal: 30 min out → 1800s.
        assert_eq!(ttl_from_expire_time("2026-08-25T00:30:00Z", now), 1800);
        // Beyond the 3600s cap → clamped to 3600.
        assert_eq!(ttl_from_expire_time("2026-08-25T05:00:00Z", now), 3600);
        // Already expired → 0 (forces refresh next call, never caches a dead token).
        assert_eq!(ttl_from_expire_time("2026-08-24T23:00:00Z", now), 0);
        // Unparseable → safe fallback to the full lifetime.
        assert_eq!(ttl_from_expire_time("not-a-timestamp", now), 3600);
    }

    #[tokio::test]
    async fn metadata_token_source_mints_chat_bot_token() {
        use wiremock::matchers::{header, method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // GCE metadata: default SA email.
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/email",
            ))
            .and(header("Metadata-Flavor", "Google"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("openab-host@dev-seba.iam.gserviceaccount.com"),
            )
            .mount(&server)
            .await;
        // GCE metadata: base access token.
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/token",
            ))
            .and(header("Metadata-Flavor", "Google"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "base-tok",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;
        // IAM Credentials: generateAccessToken (self-impersonation) → chat.bot token.
        Mock::given(method("POST"))
            .and(path_regex(
                r"/v1/projects/-/serviceAccounts/.*:generateAccessToken",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "chat-bot-tok",
                "expireTime": "2099-01-01T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let src = MetadataTokenSource::with_bases(server.uri(), server.uri());

        let token = src
            .get_token()
            .await
            .expect("should mint a chat.bot token");
        assert_eq!(token, "chat-bot-tok");
    }

    #[test]
    fn from_parts_use_adc_toggles_metadata_source() {
        let with = GoogleChatAdapter::from_parts(GoogleChatParts {
            use_adc: true,
            ..Default::default()
        });
        assert!(with.metadata_source.is_some(), "use_adc=true → ADC source");
        let without = GoogleChatAdapter::from_parts(GoogleChatParts::default());
        assert!(
            without.metadata_source.is_none(),
            "use_adc=false → no ADC source"
        );
    }

    #[test]
    fn from_parts_malformed_key_with_use_adc_installs_adc() {
        // Regression: a configured-but-malformed SA key parses to
        // token_cache=None; with use_adc=true the ADC source is still installed
        // (from_parts logs a warning naming the identity switch).
        let adapter = GoogleChatAdapter::from_parts(GoogleChatParts {
            sa_key_json: Some("not valid json".into()),
            use_adc: true,
            ..Default::default()
        });
        assert!(
            adapter.token_cache.is_none(),
            "malformed SA key → no SA-key cache"
        );
        assert!(
            adapter.metadata_source.is_some(),
            "use_adc=true → ADC source installed even when a key was configured but failed to load"
        );
    }

    #[test]
    fn from_parts_unreadable_key_file_with_use_adc_installs_adc() {
        // Regression: an unreadable/absent key FILE also yields token_cache=None
        // and must not suppress ADC.
        let adapter = GoogleChatAdapter::from_parts(GoogleChatParts {
            sa_key_file: Some("/nonexistent/path/sa-key.json".into()),
            use_adc: true,
            ..Default::default()
        });
        assert!(adapter.token_cache.is_none(), "unreadable key file → no cache");
        assert!(
            adapter.metadata_source.is_some(),
            "use_adc=true → ADC source installed when the key file could not be read"
        );
    }

    #[test]
    fn from_parts_loaded_key_suppresses_metadata_source() {
        // A successfully loaded SA key wins outright at send time, so no ADC
        // source is installed behind it (dead-at-runtime otherwise).
        let key = serde_json::json!({
            "client_email": "sa@example.iam.gserviceaccount.com",
            "private_key": "-----BEGIN PRIVATE KEY-----\nnot-a-real-key\n-----END PRIVATE KEY-----\n",
        })
        .to_string();
        let adapter = GoogleChatAdapter::from_parts(GoogleChatParts {
            sa_key_json: Some(key),
            use_adc: true,
            ..Default::default()
        });
        assert!(adapter.token_cache.is_some(), "valid key JSON → SA-key cache");
        assert!(
            adapter.metadata_source.is_none(),
            "loaded SA key → ADC source not installed"
        );
    }

    #[test]
    fn classify_generate_access_token_error_covers_documented_cases() {
        // Insufficient scope: the 403 body says "scopes" — the documented
        // signal distinguishing it from a missing IAM role.
        assert_eq!(
            classify_generate_access_token_error(
                403,
                r#"{"error":{"status":"PERMISSION_DENIED","message":"Request had insufficient authentication scopes."}}"#
            ),
            "insufficient_scope"
        );
        // Missing serviceAccountTokenCreator: 403 without the scopes wording.
        assert_eq!(
            classify_generate_access_token_error(
                403,
                r#"{"error":{"status":"IAM_PERMISSION_DENIED","message":"Permission 'iam.serviceAccounts.getAccessToken' denied on resource"}}"#
            ),
            "missing_role"
        );
        // IAM Credentials API not enabled.
        assert_eq!(
            classify_generate_access_token_error(
                403,
                r#"{"error":{"message":"IAM Service Account Credentials API has not been used in project 123 before or it is disabled."}}"#
            ),
            "api_not_enabled"
        );
        // Anything else stays unclassified rather than guessing.
        assert_eq!(classify_generate_access_token_error(500, "boom"), "unclassified");
    }

    #[tokio::test]
    async fn adc_takes_precedence_over_static_access_token() {
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/email",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("openab-host@dev-seba.iam.gserviceaccount.com"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "base-tok", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(
                r"/v1/projects/-/serviceAccounts/.*:generateAccessToken",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": "chat-bot-tok"
            })))
            .mount(&server)
            .await;

        // Adapter has BOTH an ADC source and a static token; ADC must win.
        let mut adapter = GoogleChatAdapter::from_parts(GoogleChatParts {
            access_token: Some("static-tok".into()),
            use_adc: true,
            ..Default::default()
        });
        // Repoint the ADC source at the mock server (bases are private now).
        adapter.metadata_source =
            Some(MetadataTokenSource::with_bases(server.uri(), server.uri()));
        let token = adapter.get_token().await.expect("a token");
        assert_eq!(token, "chat-bot-tok", "ADC should win over static token");
    }

    #[tokio::test]
    async fn metadata_token_source_rejects_blank_minted_token() {
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/email",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("openab-host@dev-seba.iam.gserviceaccount.com"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "base-tok", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        // A malformed IAM response with an empty accessToken must surface as a
        // mint error (and thus follow the static-token degradation path), not
        // be cached as a "valid" credential.
        Mock::given(method("POST"))
            .and(path_regex(
                r"/v1/projects/-/serviceAccounts/.*:generateAccessToken",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accessToken": ""
            })))
            .mount(&server)
            .await;

        let src = MetadataTokenSource::with_bases(server.uri(), server.uri());
        let err = src.get_token().await.expect_err("blank token must be rejected");
        assert!(
            err.contains("missing or empty accessToken"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn handle_reply_edit_message_ignores_synthetic_unified_id() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Expect ZERO requests: a synthetic `unified_<hex>` id is not a valid
        // message resource name, so the edit must be refused locally instead
        // of being sent to the API (which would 400 INVALID_ARGUMENT).
        let mock_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock_server)
            .await;

        let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "unified_a1b2c3d4e5f6".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/SP".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "updated text".into(),
            },
            command: Some("edit_message".into()),
            request_id: None,
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;
        // MockServer verifies the expect(0) on drop.
    }

    // --- Bot filtering logic test ---

    #[test]
    fn bot_user_type_detected() {
        let json = make_envelope("hello", None, "BOT", "DM", None);
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let chat = envelope.chat.unwrap();
        let sender = chat
            .message_payload
            .as_ref()
            .and_then(|p| p.message.as_ref())
            .and_then(|m| m.sender.as_ref())
            .or(chat.user.as_ref());
        let is_bot = sender.map(|s| s.user_type == "BOT").unwrap_or(false);
        assert!(is_bot);
    }

    // --- JWT verifier tests ---

    #[tokio::test]
    async fn jwt_rejects_missing_bearer_prefix() {
        let verifier = GoogleChatJwtVerifier::new("123456".into());
        let result = verifier.verify("NotBearer xyz").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Bearer"));
    }

    #[tokio::test]
    async fn jwt_rejects_invalid_token() {
        let verifier = GoogleChatJwtVerifier::new("123456".into());
        let result = verifier.verify("Bearer not.a.valid.jwt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn jwt_rejects_empty_bearer() {
        let verifier = GoogleChatJwtVerifier::new("123456".into());
        let result = verifier.verify("Bearer ").await;
        assert!(result.is_err());
    }

    #[test]
    fn email_claim_accepts_chat_system_account() {
        let claims = serde_json::json!({"email": "chat@system.gserviceaccount.com"});
        assert!(verify_email_claim(&claims).is_ok());
    }

    #[test]
    fn email_claim_rejects_other_google_email() {
        let claims = serde_json::json!({"email": "attacker@example.iam.gserviceaccount.com"});
        let err = verify_email_claim(&claims).unwrap_err();
        assert!(err.contains("email claim mismatch"));
    }

    #[test]
    fn email_claim_rejects_unrelated_gserviceaccount() {
        let claims = serde_json::json!({"email": "my-sa@my-project.iam.gserviceaccount.com"});
        assert!(verify_email_claim(&claims).is_err());
    }

    #[test]
    fn email_claim_rejects_missing_email() {
        let claims = serde_json::json!({"sub": "123", "iss": "accounts.google.com"});
        let err = verify_email_claim(&claims).unwrap_err();
        assert!(err.contains("missing email"));
    }

    #[test]
    fn email_claim_rejects_non_string_email() {
        let claims = serde_json::json!({"email": 12345});
        assert!(verify_email_claim(&claims).is_err());
    }

    #[test]
    fn human_user_type_not_filtered() {
        let json = make_envelope("hello", None, "HUMAN", "DM", None);
        let envelope: GoogleChatEnvelope = serde_json::from_str(&json).unwrap();
        let chat = envelope.chat.unwrap();
        let sender = chat
            .message_payload
            .as_ref()
            .and_then(|p| p.message.as_ref())
            .and_then(|m| m.sender.as_ref())
            .or(chat.user.as_ref());
        let is_bot = sender.map(|s| s.user_type == "BOT").unwrap_or(false);
        assert!(!is_bot);
    }

    // --- markdown_to_gchat tests ---

    #[test]
    fn markdown_bold_double_asterisk() {
        assert_eq!(markdown_to_gchat("hello **world**"), "hello *world*");
    }

    #[test]
    fn markdown_bold_underscore() {
        assert_eq!(markdown_to_gchat("hello __world__"), "hello *world*");
    }

    #[test]
    fn markdown_link_conversion() {
        assert_eq!(
            markdown_to_gchat("see [docs](https://example.com) here"),
            "see <https://example.com|docs> here"
        );
    }

    #[test]
    fn markdown_heading_to_bold() {
        assert_eq!(markdown_to_gchat("# Title\ntext"), "*Title*\ntext");
        assert_eq!(markdown_to_gchat("## Sub\ntext"), "*Sub*\ntext");
        assert_eq!(markdown_to_gchat("### Deep\ntext"), "*Deep*\ntext");
    }

    #[test]
    fn markdown_code_block_preserved() {
        let input = "before\n```rust\nlet **x** = 1;\n```\nafter **bold**";
        let output = markdown_to_gchat(input);
        assert!(output.contains("let **x** = 1;"));
        assert!(output.contains("after *bold*"));
    }

    #[test]
    fn markdown_inline_code_preserved() {
        assert_eq!(
            markdown_to_gchat("use `**not bold**` here **bold**"),
            "use `**not bold**` here *bold*"
        );
    }

    #[test]
    fn markdown_strikethrough() {
        assert_eq!(markdown_to_gchat("~~deleted~~"), "~deleted~");
        assert_eq!(
            markdown_to_gchat("keep ~~this~~ and ~~that~~"),
            "keep ~this~ and ~that~"
        );
    }

    #[test]
    fn markdown_italic_asterisk() {
        assert_eq!(markdown_to_gchat("*italic*"), "_italic_");
        assert_eq!(
            markdown_to_gchat("plain *one* and *two*"),
            "plain _one_ and _two_"
        );
    }

    #[test]
    fn markdown_italic_does_not_match_bold() {
        assert_eq!(markdown_to_gchat("**bold**"), "*bold*");
        assert_eq!(
            markdown_to_gchat("**bold** and *italic*"),
            "*bold* and _italic_"
        );
    }

    #[test]
    fn markdown_italic_underscore_passes_through() {
        // Google Chat italic is _text_, single underscore should pass through
        assert_eq!(markdown_to_gchat("_italic_"), "_italic_");
    }

    #[test]
    fn markdown_italic_no_match_when_unbalanced() {
        // Lone asterisks (no closing) should pass through
        assert_eq!(markdown_to_gchat("a * b"), "a * b");
        // Whitespace adjacent to asterisks should not match (avoid matching multiplication)
        assert_eq!(markdown_to_gchat("2 * 3 * 4"), "2 * 3 * 4");
    }

    #[test]
    fn markdown_empty_string() {
        assert_eq!(markdown_to_gchat(""), "");
    }

    #[test]
    fn markdown_no_conversion_needed() {
        assert_eq!(markdown_to_gchat("plain text"), "plain text");
    }

    #[test]
    fn markdown_multiple_links() {
        assert_eq!(
            markdown_to_gchat("[a](http://a.com) and [b](http://b.com)"),
            "<http://a.com|a> and <http://b.com|b>"
        );
    }

    #[test]
    fn markdown_nested_bold_in_link_text() {
        assert_eq!(
            markdown_to_gchat("[**bold link**](http://x.com)"),
            "<http://x.com|*bold link*>"
        );
    }

    #[test]
    fn parse_send_message_response_name() {
        let resp_json = r#"{"name": "spaces/SP1/messages/msg123", "text": "hello"}"#;
        let parsed: serde_json::Value = serde_json::from_str(resp_json).unwrap();
        let name = parsed.get("name").and_then(|v| v.as_str());
        assert_eq!(name, Some("spaces/SP1/messages/msg123"));
    }

    #[tokio::test]
    async fn handle_reply_sends_gateway_response_success() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"name": "spaces/TEST/messages/msg_abc"}),
            ))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "hello".into(),
            },
            command: None,
            request_id: Some("req_123".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected GatewayResponse on event_tx");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_123");
        assert!(resp.success);
        assert_eq!(resp.message_id, Some("spaces/TEST/messages/msg_abc".into()));
    }

    #[tokio::test]
    async fn handle_reply_sends_failure_response_on_api_error() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "hello".into(),
            },
            command: None,
            request_id: Some("req_fail".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected GatewayResponse on event_tx");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_fail");
        assert!(!resp.success);
        assert!(resp.message_id.is_none());
        let err = resp.error.expect("error should be set on send failure");
        assert!(err.contains("500"), "error should include status code, got: {}", err);
    }

    #[tokio::test]
    async fn handle_reply_empty_message_short_circuits() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        // Mount a mock that would fail the test if called
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "".into(),
            },
            command: None,
            request_id: Some("req_empty".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected failure GatewayResponse for empty message");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_empty");
        assert!(!resp.success);
        assert_eq!(resp.error, Some("empty message".into()));
    }

    #[tokio::test]
    async fn handle_reply_multi_chunk_failure_includes_error() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let long_text = "x".repeat(5000);
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: long_text,
            },
            command: None,
            request_id: Some("req_multi_fail".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected GatewayResponse");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_multi_fail");
        assert!(!resp.success);
        assert!(resp.message_id.is_none());
        let err = resp.error.expect("multi-chunk failure should set error");
        assert!(err.contains("500"));
    }

    #[tokio::test]
    async fn handle_reply_token_failure_sends_error_response() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let adapter = GoogleChatAdapter::new(None, None, None);

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "hello".into(),
            },
            command: None,
            request_id: Some("req_notoken".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected failure GatewayResponse");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_notoken");
        assert!(!resp.success);
        assert_eq!(resp.error, Some("no credentials configured".into()));
    }

    #[tokio::test]
    async fn handle_reply_edit_message_does_not_send_response() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path_regex("/spaces/.*/messages/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"name": "spaces/SP/messages/msg1"}),
            ))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "spaces/SP/messages/msg1".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/SP".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: "updated text".into(),
            },
            command: Some("edit_message".into()),
            request_id: None,
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_err());
    }

    #[tokio::test]
    async fn handle_reply_multi_chunk_sends_gateway_response() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"name": "spaces/TEST/messages/first_chunk"}),
            ))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let long_text = "x".repeat(5000);
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: long_text,
            },
            command: None,
            request_id: Some("req_multi".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected GatewayResponse for multi-chunk");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_multi");
        assert!(resp.success);
        assert_eq!(resp.message_id, Some("spaces/TEST/messages/first_chunk".into()));
    }

    #[tokio::test]
    async fn handle_reply_multi_chunk_partial_failure_reports_failure() {
        // Mixed success/failure: chunk 1 succeeds, subsequent chunks fail.
        // Expect success=false (any chunk failure marks overall as failed),
        // but message_id is still set so core has a reference.
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        // First request: 200 OK with message name
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"name": "spaces/TEST/messages/first_chunk"}),
            ))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        // Subsequent requests: 500
        Mock::given(method("POST"))
            .and(path_regex("/spaces/.*/messages"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut adapter = GoogleChatAdapter::new(None, Some("fake-token".into()), None);
        adapter.api_base = mock_server.uri();

        let long_text = "x".repeat(5000);
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "orig_msg".into(),
            platform: "googlechat".into(),
            channel: ReplyChannel {
                id: "spaces/TEST".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                attachments: Vec::new(),
                text: long_text,
            },
            command: None,
            request_id: Some("req_partial".into()),
            quote_message_id: None,
        };

        adapter.handle_reply(&reply, &event_tx).await;

        let received = event_rx.try_recv();
        assert!(received.is_ok(), "expected GatewayResponse");
        let resp: GatewayResponse = serde_json::from_str(&received.unwrap()).unwrap();
        assert_eq!(resp.request_id, "req_partial");
        assert!(!resp.success, "partial failure must report success=false");
        assert_eq!(resp.message_id, Some("spaces/TEST/messages/first_chunk".into()));
        let err = resp.error.expect("partial failure should set error");
        assert!(err.contains("500"));
    }

    // --- Attachment parsing tests ---

    fn make_attachment(
        source: &str,
        content_type: &str,
        content_name: &str,
        resource_name: Option<&str>,
    ) -> GoogleChatAttachment {
        GoogleChatAttachment {
            name: Some("spaces/SP/messages/MSG/attachments/ATT".into()),
            content_name: Some(content_name.into()),
            content_type: Some(content_type.into()),
            source: Some(source.into()),
            attachment_data_ref: resource_name.map(|rn| AttachmentDataRef {
                resource_name: Some(rn.into()),
            }),
            drive_data_ref: None,
        }
    }

    #[test]
    fn parse_attachments_image() {
        let atts = vec![make_attachment(
            "UPLOADED_CONTENT",
            "image/png",
            "photo.png",
            Some("AATT_resource"),
        )];
        let refs = parse_attachments(&atts);
        assert_eq!(refs.len(), 1);
        match &refs[0] {
            GoogleChatMediaRef::Image {
                resource_name,
                content_name,
            } => {
                assert_eq!(resource_name, "AATT_resource");
                assert_eq!(content_name, "photo.png");
            }
            other => panic!("expected Image, got {:?}", other),
        }
    }

    #[test]
    fn parse_attachments_audio() {
        let atts = vec![make_attachment(
            "UPLOADED_CONTENT",
            "audio/mp4",
            "voice.m4a",
            Some("AATT"),
        )];
        let refs = parse_attachments(&atts);
        assert!(matches!(refs[0], GoogleChatMediaRef::Audio { .. }));
    }

    #[test]
    fn parse_attachments_file() {
        let atts = vec![make_attachment(
            "UPLOADED_CONTENT",
            "text/plain",
            "notes.txt",
            Some("AATT"),
        )];
        let refs = parse_attachments(&atts);
        assert!(matches!(refs[0], GoogleChatMediaRef::File { .. }));
    }

    #[test]
    fn parse_attachments_skips_drive() {
        let atts = vec![GoogleChatAttachment {
            name: Some("spaces/SP/messages/MSG/attachments/ATT".into()),
            content_name: Some("doc".into()),
            content_type: Some("application/vnd.google-apps.document".into()),
            source: Some("DRIVE_FILE".into()),
            attachment_data_ref: None,
            drive_data_ref: Some(DriveDataRef {
                drive_file_id: Some("drive_id_123".into()),
            }),
        }];
        assert_eq!(parse_attachments(&atts).len(), 0);
    }

    #[test]
    fn parse_attachments_skips_missing_resource_name() {
        let atts = vec![make_attachment(
            "UPLOADED_CONTENT",
            "image/png",
            "photo.png",
            None,
        )];
        assert_eq!(parse_attachments(&atts).len(), 0);
    }

    #[test]
    fn media_url_preserves_slashes_and_encodes_specials() {
        let url = media_url("https://chat.googleapis.com/v1", "spaces/SP/messages/MSG/attachments/ATT");
        assert_eq!(
            url,
            "https://chat.googleapis.com/v1/media/spaces/SP/messages/MSG/attachments/ATT?alt=media"
        );
        let url2 = media_url("https://chat.googleapis.com/v1", "AATT/some+resource=name");
        assert_eq!(
            url2,
            "https://chat.googleapis.com/v1/media/AATT/some%2Bresource%3Dname?alt=media"
        );
    }

    #[tokio::test]
    async fn download_googlechat_image_resizes_and_returns_attachment() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        // Generate a small valid PNG
        let img = image::RgbImage::from_pixel(10, 10, image::Rgb([255, 0, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let png_bytes = buf.into_inner();

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/media/.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png_bytes)
                    .insert_header("content-type", "image/png"),
            )
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let result = download_googlechat_image(
            &client,
            "fake-token",
            &mock_server.uri(),
            "AATT_resource",
            "photo.png",
        )
        .await;
        let att = result;
        assert_eq!(att.attachment_type, "image");
        assert_eq!(att.filename, "photo.png");
        assert_eq!(att.mime_type, "image/jpeg"); // resized PNG → JPEG
        assert!(att.path.is_some()); // stored to filesystem
        assert!(att.size > 0);
    }

    #[tokio::test]
    async fn download_googlechat_file_rejects_non_text_extension() {
        let client = reqwest::Client::new();
        let result = download_googlechat_file(
            &client,
            "fake-token",
            "https://unused", // not called for non-text
            "AATT",
            "binary.exe",
            TEXT_TOTAL_CAP,
        )
        .await;
        let att = result;
        assert!(att.status.is_some(), "non-text extension must have status set");
        let reason = att.status.unwrap();
        assert!(reason.contains("unsupported format"), "got: {reason}");
    }

    #[tokio::test]
    async fn download_googlechat_file_text_extension_succeeds() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/media/.*"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"hello world".to_vec()),
            )
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let result = download_googlechat_file(
            &client,
            "fake-token",
            &mock_server.uri(),
            "AATT",
            "notes.txt",
            TEXT_TOTAL_CAP,
        )
        .await;
        let att = result;
        assert_eq!(att.attachment_type, "text_file");
        assert_eq!(att.filename, "notes.txt");
        assert_eq!(att.mime_type, "text/plain");
    }

    #[tokio::test]
    async fn download_googlechat_audio_returns_attachment() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        let audio_bytes = vec![0u8; 1024];
        Mock::given(method("GET"))
            .and(path_regex("/media/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(audio_bytes.clone()))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let result = download_googlechat_audio(
            &client,
            "fake-token",
            &mock_server.uri(),
            "AATT",
            "voice.m4a",
            "audio/mp4",
        )
        .await;
        let att = result;
        assert_eq!(att.attachment_type, "audio");
        assert_eq!(att.filename, "voice.m4a");
        assert_eq!(att.mime_type, "audio/mp4");
        assert_eq!(att.size, 1024);
    }

    #[tokio::test]
    async fn download_googlechat_image_rejects_oversized_content_length() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("/media/.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", "20000000") // 20 MB > 10 MB limit
                    .set_body_bytes(vec![0u8; 100]),
            )
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let result = download_googlechat_image(
            &client,
            "fake-token",
            &mock_server.uri(),
            "AATT",
            "huge.png",
        )
        .await;
        let att = result;
        assert!(att.status.is_some(), "oversized image must have status set");
        let reason = att.status.unwrap();
        // Either the Content-Length check fires ("size exceeded") or the body read fails
        // ("download failed") — both are valid rejections; the key invariant is
        // that a rejected attachment is returned rather than None/silent drop.
        assert!(
            reason.contains("size exceeded") || reason.contains("download failed"),
            "got: {reason}"
        );
    }

    #[test]
    fn parses_http_endpoint_url_top_level_envelope() {
        let envelope: GoogleChatEnvelope = serde_json::from_value(serde_json::json!({
            "message": {
                "name": "spaces/AAAA/messages/BBBB",
                "text": "hello",
                "attachment": []
            },
            "user": {
                "name": "users/123",
                "displayName": "Test User",
                "type": "HUMAN"
            },
            "space": {
                "name": "spaces/AAAA",
                "type": "DM"
            }
        }))
        .unwrap();
        assert!(envelope.chat.is_none());
        assert!(envelope.message.is_some());
        assert_eq!(envelope.message.unwrap().name, "spaces/AAAA/messages/BBBB");
        assert!(envelope.user.is_some());
        assert_eq!(envelope.user.unwrap().name, "users/123");
        assert!(envelope.space.is_some());
        assert_eq!(envelope.space.unwrap().name, "spaces/AAAA");
    }
}

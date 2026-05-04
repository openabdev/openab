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

// --- Google Chat types (v2 envelope format) ---

#[derive(Debug, Deserialize)]
pub struct GoogleChatEnvelope {
    pub chat: Option<ChatPayload>,
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
    #[allow(dead_code)]
    pub space_type_renamed: Option<String>,
}

// --- Webhook JWT verification ---

const GOOGLE_CHAT_ISSUER: &str = "https://accounts.google.com";
const GOOGLE_CHAT_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const GOOGLE_CHAT_EMAIL_SUFFIX: &str = "@gcp-sa-gsuiteaddons.iam.gserviceaccount.com";
const JWKS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Verify the JWT's `email` claim belongs to a Google Chat service account.
/// Google Chat webhooks use `service-{PROJECT_NUMBER}@gcp-sa-gsuiteaddons.iam.gserviceaccount.com`.
/// Without this check, any Google-issued ID token would be accepted.
fn verify_email_claim(claims: &serde_json::Value) -> Result<(), String> {
    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or("missing email claim")?;
    if !email.ends_with(GOOGLE_CHAT_EMAIL_SUFFIX) {
        return Err(format!(
            "email claim mismatch: expected *{GOOGLE_CHAT_EMAIL_SUFFIX}, got {email}"
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
    pub access_token: Option<String>,
    pub jwt_verifier: Option<GoogleChatJwtVerifier>,
    pub client: reqwest::Client,
}

impl GoogleChatAdapter {
    pub fn new(
        token_cache: Option<GoogleChatTokenCache>,
        access_token: Option<String>,
        jwt_verifier: Option<GoogleChatJwtVerifier>,
    ) -> Self {
        Self {
            token_cache,
            access_token,
            jwt_verifier,
            client: reqwest::Client::new(),
        }
    }

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
        self.access_token.clone()
    }

    pub async fn handle_reply(&self, reply: &GatewayReply) {
        if matches!(
            reply.command.as_deref(),
            Some("add_reaction") | Some("remove_reaction") | Some("create_topic")
        ) {
            return;
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
            return;
        };

        let text = &reply.content.text;
        let chunks = split_text(text, GOOGLE_CHAT_MESSAGE_LIMIT);

        for chunk in chunks {
            send_message(
                &self.client,
                &token,
                &reply.channel.id,
                reply.channel.thread_id.as_deref(),
                chunk,
            )
            .await;
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

    let Some(chat) = envelope.chat else {
        return empty_json_response();
    };
    let Some(payload) = chat.message_payload else {
        return empty_json_response();
    };
    let Some(ref msg) = payload.message else {
        return empty_json_response();
    };

    let text = msg
        .argument_text
        .as_deref()
        .or(msg.text.as_deref())
        .unwrap_or("");
    if text.trim().is_empty() {
        return empty_json_response();
    }

    let sender = msg.sender.as_ref().or(chat.user.as_ref());
    let space = msg.space.as_ref().or(payload.space.as_ref());

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

    let gw_event = GatewayEvent::new(
        "googlechat",
        ChannelInfo {
            id: space_name.clone(),
            channel_type: space_type,
            thread_id,
        },
        SenderInfo {
            id: sender_id,
            name: sender_name.clone(),
            display_name,
            is_bot: false,
        },
        text,
        &message_id,
        vec![],
    );

    let json = serde_json::to_string(&gw_event).unwrap();
    info!(space = %space_name, sender = %sender_name, "googlechat → gateway");
    let _ = state.event_tx.send(json);
    empty_json_response()
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
        let (new_token, expire) = self.refresh(client).await?;
        *guard = Some((new_token.clone(), Instant::now(), expire));
        info!("googlechat access token refreshed (expires in {expire}s)");
        Ok(new_token)
    }

    async fn refresh(&self, client: &reqwest::Client) -> Result<(String, u64), String> {
        let jwt = self.build_jwt().map_err(|e| format!("JWT build error: {e}"))?;
        let resp = client
            .post("https://oauth2.googleapis.com/token")
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
            .ok_or_else(|| {
                let err = body
                    .get("error_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                format!("token exchange failed: {err}")
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

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    space: &str,
    thread_id: Option<&str>,
    text: &str,
) {
    let mut url = format!("{}/{}/messages", GOOGLE_CHAT_API_BASE, space);

    let mut body = serde_json::json!({
        "text": text,
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
        Ok(r) if !r.status().is_success() => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "googlechat send error");
        }
        Err(e) => error!("googlechat send error: {e}"),
        _ => {}
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
    fn email_claim_accepts_gsuite_addons_account() {
        let claims = serde_json::json!({"email": "service-123456@gcp-sa-gsuiteaddons.iam.gserviceaccount.com"});
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
}

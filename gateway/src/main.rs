use anyhow::Result;
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

// --- Event schema (ADR openab.gateway.event.v1) ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayEvent {
    pub schema: String,
    pub event_id: String,
    pub timestamp: String,
    pub platform: String,
    pub event_type: String,
    pub channel: ChannelInfo,
    pub sender: SenderInfo,
    pub content: Content,
    pub mentions: Vec<String>,
    pub message_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SenderInfo {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Content {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

// --- Reply schema (ADR openab.gateway.reply.v1) ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayReply {
    pub schema: String,
    pub reply_to: String,
    pub platform: String,
    pub channel: ReplyChannel,
    pub content: Content,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyChannel {
    pub id: String,
    pub thread_id: Option<String>,
}

/// Response from gateway back to OAB for commands (e.g. create_topic)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayResponse {
    pub schema: String,
    pub request_id: String,
    pub success: bool,
    pub thread_id: Option<String>,
    pub error: Option<String>,
}

// --- Telegram types (minimal) ---

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    message_thread_id: Option<i64>,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
    #[serde(default)]
    entities: Vec<TelegramEntity>,
}

#[derive(Debug, Deserialize)]
struct TelegramEntity {
    #[serde(rename = "type")]
    entity_type: String,
    offset: usize,
    length: usize,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
    is_forum: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    first_name: String,
    last_name: Option<String>,
    username: Option<String>,
    is_bot: bool,
}

// --- App state ---

/// Cache entry for LINE reply tokens: (replyToken, insertion_time).
/// Uses std::sync::Mutex — critical sections are short (insert/remove/retain)
/// and never held across .await, so async Mutex overhead is unnecessary.
type ReplyTokenCache = Arc<std::sync::Mutex<std::collections::HashMap<String, (String, Instant)>>>;

/// Maximum age (in seconds) before a cached reply token is considered expired.
/// LINE tokens are valid for ~1 minute; we use 50s as a conservative margin.
const REPLY_TOKEN_TTL_SECS: u64 = 50;

/// Maximum number of cached reply tokens. Prevents unbounded memory growth
/// if webhooks arrive faster than OAB can reply (e.g. OAB offline, spam burst).
const REPLY_TOKEN_CACHE_MAX: usize = 10_000;

struct AppState {
    bot_token: String,
    secret_token: Option<String>,
    ws_token: Option<String>,
    line_channel_secret: Option<String>,
    line_access_token: Option<String>,
    /// Broadcast channel: gateway → OAB (events)
    event_tx: broadcast::Sender<String>,
    /// Cache: event_id → (LINE replyToken, timestamp).
    /// Global across all OAB WebSocket clients. LINE reply tokens are single-use:
    /// the first client to `remove()` a token wins the free Reply API call;
    /// other clients for the same event naturally fall back to Push API.
    reply_token_cache: ReplyTokenCache,
}

// --- Telegram webhook handler ---

async fn telegram_webhook(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(update): Json<TelegramUpdate>,
) -> axum::http::StatusCode {
    // Validate secret_token if configured
    if let Some(ref expected) = state.secret_token {
        let provided = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok());
        if provided != Some(expected.as_str()) {
            warn!("webhook rejected: invalid or missing secret_token");
            return axum::http::StatusCode::UNAUTHORIZED;
        }
    }
    let Some(msg) = update.message else {
        return axum::http::StatusCode::OK;
    };
    let Some(text) = msg.text.as_deref() else {
        return axum::http::StatusCode::OK;
    };
    // Skip empty messages
    if text.trim().is_empty() {
        return axum::http::StatusCode::OK;
    }

    let from = msg.from.as_ref();
    let sender_name = from
        .and_then(|u| u.username.as_deref())
        .unwrap_or("unknown");
    let display_name = from
        .map(|u| {
            let mut n = u.first_name.clone();
            if let Some(last) = &u.last_name {
                n.push(' ');
                n.push_str(last);
            }
            n
        })
        .unwrap_or_else(|| "Unknown".into());

    // Extract @mentions from entities
    let mentions: Vec<String> = msg
        .entities
        .iter()
        .filter(|e| e.entity_type == "mention")
        .filter_map(|e| {
            text.get(e.offset..e.offset + e.length)
                .map(|s| s.trim_start_matches('@').to_string())
        })
        .collect();

    let event = GatewayEvent {
        schema: "openab.gateway.event.v1".into(),
        event_id: format!("evt_{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now().to_rfc3339(),
        platform: "telegram".into(),
        event_type: "message".into(),
        channel: ChannelInfo {
            id: msg.chat.id.to_string(),
            channel_type: msg.chat.chat_type.clone(),
            thread_id: msg.message_thread_id.map(|id| id.to_string()),
        },
        sender: SenderInfo {
            id: from.map(|u| u.id.to_string()).unwrap_or_default(),
            name: sender_name.into(),
            display_name,
            is_bot: from.map(|u| u.is_bot).unwrap_or(false),
        },
        content: Content {
            content_type: "text".into(),
            text: text.into(),
        },
        mentions,
        message_id: msg.message_id.to_string(),
    };

    let json = serde_json::to_string(&event).unwrap();
    info!(chat_id = %msg.chat.id, sender = %sender_name, "telegram → gateway");
    let _ = state.event_tx.send(json);
    axum::http::StatusCode::OK
}

// --- LINE types ---

#[derive(Debug, Deserialize)]
struct LineWebhookBody {
    events: Vec<LineEvent>,
}

#[derive(Debug, Deserialize)]
struct LineEvent {
    #[serde(rename = "type")]
    event_type: String,
    source: Option<LineSource>,
    message: Option<LineMessage>,
    #[serde(rename = "replyToken")]
    reply_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    text: Option<String>,
}

// --- LINE webhook handler ---

async fn line_webhook(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    // Validate X-Line-Signature
    if let Some(ref channel_secret) = state.line_channel_secret {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let signature = headers
            .get("x-line-signature")
            .and_then(|v| v.to_str().ok());
        let Some(signature) = signature else {
            warn!("LINE webhook rejected: missing X-Line-Signature");
            return axum::http::StatusCode::UNAUTHORIZED;
        };

        let mut mac = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()).expect("HMAC key");
        mac.update(&body);
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        if signature != expected {
            warn!("LINE webhook rejected: invalid signature");
            return axum::http::StatusCode::UNAUTHORIZED;
        }
    }

    let webhook_body: LineWebhookBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            warn!("LINE webhook parse error: {e}");
            return axum::http::StatusCode::BAD_REQUEST;
        }
    };

    for event in webhook_body.events {
        if event.event_type != "message" {
            continue;
        }
        let Some(ref msg) = event.message else {
            continue;
        };
        if msg.message_type != "text" {
            continue;
        }
        let Some(ref text) = msg.text else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }

        let source = event.source.as_ref();
        let (channel_id, channel_type) = match source {
            Some(s) if s.source_type == "group" => {
                (s.group_id.clone().unwrap_or_default(), "group".to_string())
            }
            Some(s) if s.source_type == "room" => {
                (s.room_id.clone().unwrap_or_default(), "room".to_string())
            }
            Some(s) => (s.user_id.clone().unwrap_or_default(), "user".to_string()),
            None => continue,
        };
        let user_id = source
            .and_then(|s| s.user_id.as_deref())
            .unwrap_or("unknown");

        let event_id = format!("evt_{}", uuid::Uuid::new_v4());

        // Cache the reply token for hybrid Reply/Push dispatch
        if let Some(ref reply_token) = event.reply_token {
            let mut cache = state.reply_token_cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= REPLY_TOKEN_CACHE_MAX {
                warn!(size = cache.len(), "reply token cache full, skipping insert");
            } else {
                cache.insert(event_id.clone(), (reply_token.clone(), Instant::now()));
                info!(event_id = %event_id, "cached LINE replyToken");
            }
        }

        let gateway_event = GatewayEvent {
            schema: "openab.gateway.event.v1".into(),
            event_id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            platform: "line".into(),
            event_type: "message".into(),
            channel: ChannelInfo {
                id: channel_id.clone(),
                channel_type,
                thread_id: None,
            },
            sender: SenderInfo {
                id: user_id.into(),
                name: user_id.into(),
                display_name: user_id.into(),
                is_bot: false,
            },
            content: Content {
                content_type: "text".into(),
                text: text.clone(),
            },
            mentions: vec![],
            message_id: msg.id.clone(),
        };

        let json = serde_json::to_string(&gateway_event).unwrap();
        info!(channel = %channel_id, sender = %user_id, "line → gateway");
        let _ = state.event_tx.send(json);
    }

    axum::http::StatusCode::OK
}

// --- WebSocket handler (OAB connects here) ---

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    // Validate WS token if configured
    if let Some(ref expected) = state.ws_token {
        let provided = query.get("token").map(|s| s.as_str());
        if provided != Some(expected.as_str()) {
            warn!("WebSocket rejected: invalid or missing token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_oab_connection(state, socket))
}

async fn handle_oab_connection(state: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();

    // Channel for replies from this OAB client
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<GatewayReply>(64);

    info!("OAB client connected via WebSocket");

    // Forward gateway events → OAB
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(event_json) = event_rx.recv() => {
                    if ws_tx.send(Message::Text(event_json.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Receive OAB replies → Telegram
    let bot_token = state.bot_token.clone();
    let line_access_token = state.line_access_token.clone();
    let reply_cache = state.reply_token_cache.clone();
    let event_tx_for_recv = state.event_tx.clone();
    // Track per-message reaction state (Telegram replaces all reactions atomically)
    let reaction_state: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let recv_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<GatewayReply>(&text) {
                    Ok(reply) => {
                        // Handle create_topic command
                        if reply.command.as_deref() == Some("create_topic") {
                            let req_id = reply.request_id.clone().unwrap_or_default();
                            info!(chat_id = %reply.channel.id, "creating forum topic");
                            let url = format!(
                                "https://api.telegram.org/bot{}/createForumTopic",
                                bot_token
                            );
                            let resp = client
                                .post(&url)
                                .json(&serde_json::json!({
                                    "chat_id": reply.channel.id,
                                    "name": reply.content.text,
                                }))
                                .send()
                                .await;
                            let gw_resp = match resp {
                                Ok(r) => {
                                    let body: serde_json::Value =
                                        r.json().await.unwrap_or_default();
                                    if body["ok"].as_bool() == Some(true) {
                                        let tid = body["result"]["message_thread_id"]
                                            .as_i64()
                                            .map(|id| id.to_string());
                                        info!(thread_id = ?tid, "forum topic created");
                                        GatewayResponse {
                                            schema: "openab.gateway.response.v1".into(),
                                            request_id: req_id,
                                            success: true,
                                            thread_id: tid,
                                            error: None,
                                        }
                                    } else {
                                        let err = body["description"]
                                            .as_str()
                                            .unwrap_or("unknown error")
                                            .to_string();
                                        warn!(err = %err, "createForumTopic failed");
                                        GatewayResponse {
                                            schema: "openab.gateway.response.v1".into(),
                                            request_id: req_id,
                                            success: false,
                                            thread_id: None,
                                            error: Some(err),
                                        }
                                    }
                                }
                                Err(e) => GatewayResponse {
                                    schema: "openab.gateway.response.v1".into(),
                                    request_id: req_id,
                                    success: false,
                                    thread_id: None,
                                    error: Some(e.to_string()),
                                },
                            };
                            // Send response back — need to use event_tx broadcast
                            let json = serde_json::to_string(&gw_resp).unwrap();
                            let _ = event_tx_for_recv.send(json);
                            continue;
                        }

                        // Handle add_reaction / remove_reaction
                        // Telegram setMessageReaction replaces ALL reactions, so we track state
                        if reply.command.as_deref() == Some("add_reaction")
                            || reply.command.as_deref() == Some("remove_reaction")
                        {
                            let msg_key = format!("{}:{}", reply.channel.id, reply.reply_to);
                            let emoji = &reply.content.text;
                            // Map unsupported emojis to Telegram-compatible ones
                            let tg_emoji = match emoji.as_str() {
                                "🆗" => "👍",
                                other => other,
                            };
                            let is_add = reply.command.as_deref() == Some("add_reaction");
                            {
                                let mut reactions = reaction_state.lock().await;
                                let set = reactions.entry(msg_key.clone()).or_insert_with(Vec::new);
                                if is_add {
                                    if !set.contains(&tg_emoji.to_string()) {
                                        set.push(tg_emoji.to_string());
                                    }
                                } else {
                                    set.retain(|e| e != tg_emoji);
                                }
                            }
                            let current: Vec<serde_json::Value> = {
                                let reactions = reaction_state.lock().await;
                                reactions
                                    .get(&msg_key)
                                    .map(|v| {
                                        v.iter()
                                            .map(|e| {
                                                serde_json::json!({"type": "emoji", "emoji": e})
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            };
                            let url = format!(
                                "https://api.telegram.org/bot{}/setMessageReaction",
                                bot_token
                            );
                            let _ = client
                                .post(&url)
                                .json(&serde_json::json!({
                                    "chat_id": reply.channel.id,
                                    "message_id": reply.reply_to,
                                    "reaction": current,
                                }))
                                .send()
                                .await
                                .map_err(|e| error!("telegram reaction error: {e}"));
                            continue;
                        }

                        // Normal send_message — route by platform
                        if reply.platform == "line" {
                            if let Some(ref access_token) = line_access_token {
                                // Extract token from cache (drop lock before HTTP call)
                                let cached_token = {
                                    let mut cache = reply_cache.lock().unwrap_or_else(|e| e.into_inner());
                                    cache
                                        .remove(&reply.reply_to)
                                        .and_then(|(token, cached_at)| {
                                            if cached_at.elapsed().as_secs() < REPLY_TOKEN_TTL_SECS
                                            {
                                                Some(token)
                                            } else {
                                                info!("LINE replyToken expired, using Push API");
                                                None
                                            }
                                        })
                                };

                                // Try Reply API first (free, no quota consumed)
                                let mut used_reply = false;
                                if let Some(reply_token) = cached_token {
                                    info!(to = %reply.channel.id, "gateway → line (reply API)");
                                    let resp = client
                                        .post("https://api.line.me/v2/bot/message/reply")
                                        .bearer_auth(access_token)
                                        .json(&serde_json::json!({
                                            "replyToken": reply_token,
                                            "messages": [{"type": "text", "text": reply.content.text}]
                                        }))
                                        .send()
                                        .await;
                                    match resp {
                                        Ok(r) if r.status().is_success() => {
                                            used_reply = true;
                                        }
                                        Ok(r) => {
                                            let status = r.status();
                                            let body = r.text().await.unwrap_or_default();
                                            // Only fallback to Push when LINE explicitly says
                                            // the reply token is unusable (invalid/expired).
                                            // LINE returns "Invalid reply token" or "expired"
                                            // in the error body for token-specific failures.
                                            let body_lower = body.to_lowercase();
                                            let token_unusable = status.as_u16() == 400
                                                && ((body_lower.contains("invalid")
                                                    && body_lower.contains("reply token"))
                                                    || body_lower.contains("expired"));
                                            if token_unusable {
                                                warn!(status = %status, body = %body, "LINE reply token unusable, falling back to Push");
                                            } else {
                                                // Ambiguous: 5xx, other 4xx, or unrecognized 400.
                                                // Message may have been delivered — do NOT fallback.
                                                error!(status = %status, body = %body, "LINE Reply API error, NOT falling back to Push (possible duplicate risk)");
                                                used_reply = true;
                                            }
                                        }
                                        Err(e) => {
                                            // Network/timeout error: delivery ambiguous, do NOT fallback
                                            error!(err = %e, "LINE Reply API network error, NOT falling back to Push (possible duplicate risk)");
                                            used_reply = true;
                                        }
                                    }
                                }

                                // Fallback to Push API
                                if !used_reply {
                                    info!(to = %reply.channel.id, "gateway → line (push API)");
                                    let _ = client
                                        .post("https://api.line.me/v2/bot/message/push")
                                        .bearer_auth(access_token)
                                        .json(&serde_json::json!({
                                            "to": reply.channel.id,
                                            "messages": [{"type": "text", "text": reply.content.text}]
                                        }))
                                        .send()
                                        .await
                                        .map_err(|e| error!("line push error: {e}"));
                                }
                            }
                        } else {
                            // Telegram sendMessage
                            info!(chat_id = %reply.channel.id, thread_id = ?reply.channel.thread_id, "gateway → telegram");
                            let url =
                                format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
                            let _ = client
                                .post(&url)
                                .json(&serde_json::json!({
                                    "chat_id": reply.channel.id,
                                    "text": reply.content.text,
                                    "message_thread_id": reply.channel.thread_id,
                                    "parse_mode": "Markdown",
                                }))
                                .send()
                                .await
                                .map_err(|e| error!("telegram send error: {e}"));
                        }
                    }
                    Err(e) => warn!("invalid reply from OAB: {e}"),
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
    info!("OAB client disconnected");
}

// --- Health check ---

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN must be set");
    let secret_token = std::env::var("TELEGRAM_SECRET_TOKEN").ok();
    let ws_token = std::env::var("GATEWAY_WS_TOKEN").ok();
    let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok();
    let line_access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();
    let listen_addr = std::env::var("GATEWAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let webhook_path =
        std::env::var("TELEGRAM_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/telegram".into());

    if secret_token.is_none() {
        warn!("TELEGRAM_SECRET_TOKEN not set — webhook requests are NOT validated (insecure)");
    }
    if ws_token.is_none() {
        warn!("GATEWAY_WS_TOKEN not set — WebSocket connections are NOT authenticated (insecure)");
    }

    let (event_tx, _) = broadcast::channel::<String>(256);
    let reply_token_cache: ReplyTokenCache =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    let state = Arc::new(AppState {
        bot_token,
        secret_token,
        ws_token,
        line_channel_secret,
        line_access_token,
        event_tx,
        reply_token_cache,
    });

    // Background task: sweep expired reply tokens every REPLY_TOKEN_TTL_SECS
    {
        let cache_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(REPLY_TOKEN_TTL_SECS)).await;
                let mut cache = cache_state.reply_token_cache.lock().unwrap_or_else(|e| e.into_inner());
                let before = cache.len();
                cache.retain(|_, (_, t)| t.elapsed().as_secs() < REPLY_TOKEN_TTL_SECS);
                let after = cache.len();
                if before != after {
                    info!(
                        removed = before - after,
                        remaining = after,
                        "reply token cache sweep"
                    );
                }
            }
        });
    }

    let app = Router::new()
        .route(&webhook_path, post(telegram_webhook))
        .route("/webhook/line", post(line_webhook))
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .with_state(state);

    info!(addr = %listen_addr, webhook = %webhook_path, "gateway starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

use anyhow::Result;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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

struct AppState {
    bot_token: String,
    /// Broadcast channel: gateway → OAB (events)
    event_tx: broadcast::Sender<String>,
    /// Collected reply senders from connected OAB clients
    reply_handlers: Mutex<Vec<tokio::sync::mpsc::Sender<GatewayReply>>>,
}

// --- Telegram webhook handler ---

async fn telegram_webhook(
    State(state): State<Arc<AppState>>,
    Json(update): Json<TelegramUpdate>,
) -> &'static str {
    let Some(msg) = update.message else {
        return "ok";
    };
    let Some(text) = msg.text.as_deref() else {
        return "ok";
    };
    // Skip empty messages
    if text.trim().is_empty() {
        return "ok";
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
        message_id: msg.message_id.to_string(),
    };

    let json = serde_json::to_string(&event).unwrap();
    info!(chat_id = %msg.chat.id, sender = %sender_name, "telegram → gateway");
    let _ = state.event_tx.send(json);
    "ok"
}

// --- WebSocket handler (OAB connects here) ---

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    ws.on_upgrade(move |socket| handle_oab_connection(state, socket))
}

async fn handle_oab_connection(state: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();

    // Channel for replies from this OAB client
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<GatewayReply>(64);
    state.reply_handlers.lock().await.push(reply_tx);

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
                // No reply forwarding needed on this path — replies go to Telegram directly
            }
        }
    });

    // Receive OAB replies → Telegram
    let bot_token = state.bot_token.clone();
    let event_tx_for_recv = state.event_tx.clone();
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
                                    let body: serde_json::Value = r.json().await.unwrap_or_default();
                                    if body["ok"].as_bool() == Some(true) {
                                        let tid = body["result"]["message_thread_id"].as_i64()
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
                                        let err = body["description"].as_str()
                                            .unwrap_or("unknown error").to_string();
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

                        // Normal send_message
                        info!(chat_id = %reply.channel.id, thread_id = ?reply.channel.thread_id, "gateway → telegram");
                        let url = format!(
                            "https://api.telegram.org/bot{}/sendMessage",
                            bot_token
                        );
                        let _ = client
                            .post(&url)
                            .json(&serde_json::json!({
                                "chat_id": reply.channel.id,
                                "text": reply.content.text,
                                "message_thread_id": reply.channel.thread_id,
                            }))
                            .send()
                            .await
                            .map_err(|e| error!("telegram send error: {e}"));
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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .expect("TELEGRAM_BOT_TOKEN must be set");
    let listen_addr = std::env::var("GATEWAY_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8080".into());
    let webhook_path = std::env::var("TELEGRAM_WEBHOOK_PATH")
        .unwrap_or_else(|_| "/webhook/telegram".into());

    let (event_tx, _) = broadcast::channel::<String>(256);

    let state = Arc::new(AppState {
        bot_token,
        event_tx,
        reply_handlers: Mutex::new(Vec::new()),
    });

    let app = Router::new()
        .route(&webhook_path, post(telegram_webhook))
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .with_state(state);

    info!(addr = %listen_addr, webhook = %webhook_path, "gateway starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

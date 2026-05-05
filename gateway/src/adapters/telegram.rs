use crate::schema::*;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Base URL for Telegram Bot API. Extracted as constant for consistency
/// with LINE's `LINE_API_BASE` and to enable future mock testing.
pub const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

// --- Telegram types ---

#[derive(Debug, Deserialize)]
pub struct TelegramUpdate {
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
    photo: Option<Vec<TelegramPhotoSize>>,
    voice: Option<TelegramVoice>,
    audio: Option<TelegramAudio>,
    caption: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TelegramAudio {
    file_id: String,
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
    #[allow(dead_code)]
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

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    Json(update): Json<TelegramUpdate>,
) -> axum::http::StatusCode {
    if let Some(ref expected) = state.telegram_secret_token {
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

    let mut text = msg
        .text
        .clone()
        .unwrap_or_else(|| msg.caption.clone().unwrap_or_default());
    let mut attachments = vec![];

    // Handle Image/Audio attachments (Issue #690 Phase 1)
    let media_info = if let Some(ref photos) = msg.photo {
        photos.last().map(|p| (p.file_id.clone(), "image"))
    } else {
        msg.voice
            .as_ref()
            .map(|v| (v.file_id.clone(), "audio"))
            .or_else(|| msg.audio.as_ref().map(|a| (a.file_id.clone(), "audio")))
    };

    if let (Some((file_id, m_type)), Some(ref token)) = (media_info, &state.telegram_bot_token) {
        let client = reqwest::Client::new();
        // 1. getFile to get file_path
        let url = format!("{TELEGRAM_API_BASE}/bot{token}/getFile?file_id={file_id}");
        if let Ok(resp) = client.get(url).send().await {
            if !resp.status().is_success() {
                warn!(status = %resp.status(), id = %file_id, "Telegram getFile failed");
            } else if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(file_path) = body["result"]["file_path"].as_str() {
                    // 2. Download the file
                    let download_url = format!("{TELEGRAM_API_BASE}/file/bot{token}/{file_path}");
                    if let Ok(r) = client.get(download_url).send().await {
                        if !r.status().is_success() {
                            warn!(status = %r.status(), id = %file_id, "failed to download Telegram media");
                        } else {
                            // Issue #690 review fix: Check file size before downloading
                            let content_length = r
                                .headers()
                                .get("content-length")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);

                            if content_length > state.media_max_file_size {
                                warn!(
                                    size = content_length,
                                    max = state.media_max_file_size,
                                    id = %file_id,
                                    "Telegram media too large, skipping"
                                );
                            } else {
                                let mime = r
                                    .headers()
                                    .get("content-type")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or(if m_type == "image" {
                                        "image/jpeg"
                                    } else {
                                        "audio/ogg"
                                    })
                                    .to_string();

                                if let Ok(data) = r.bytes().await {
                                    let uuid = uuid::Uuid::new_v4().to_string();
                                    let size = data.len() as u64;
                                    let proxied = {
                                        let mut store =
                                            state.media_store.lock().unwrap_or_else(|e| e.into_inner());
                                        if store.len() >= state.media_max_entries {
                                            warn!(
                                                size = store.len(),
                                                "media store full, skipping Telegram media proxy"
                                            );
                                            false
                                        } else {
                                            store.insert(
                                                uuid.clone(),
                                                (
                                                    data.to_vec(),
                                                    mime.clone(),
                                                    std::time::Instant::now(),
                                                ),
                                            );
                                            true
                                        }
                                    };
                                    if proxied {
                                        attachments.push(Attachment {
                                            attachment_type: m_type.into(),
                                            url: format!("{}/media/{}", state.public_url, uuid),
                                            mime_type: Some(mime),
                                            filename: Some(format!(
                                                "telegram-{}.{}",
                                                file_id,
                                                if m_type == "image" { "jpg" } else { "ogg" }
                                            )),
                                            size: Some(size),
                                        });
                                        if text.is_empty() {
                                            text = format!("[{}]", m_type);
                                        }
                                        info!(id = %file_id, uuid = %uuid, "proxied Telegram inbound media");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if text.trim().is_empty() && attachments.is_empty() {
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

    let mentions: Vec<String> = msg
        .entities
        .iter()
        .filter(|e| e.entity_type == "mention")
        .filter_map(|e| {
            text.get(e.offset..e.offset + e.length)
                .map(|s| s.trim_start_matches('@').to_string())
        })
        .collect();

    let mut event = GatewayEvent::new(
        "telegram",
        ChannelInfo {
            id: msg.chat.id.to_string(),
            channel_type: msg.chat.chat_type.clone(),
            thread_id: msg.message_thread_id.map(|id| id.to_string()),
        },
        SenderInfo {
            id: from.map(|u| u.id.to_string()).unwrap_or_default(),
            name: sender_name.into(),
            display_name,
            is_bot: from.map(|u| u.is_bot).unwrap_or(false),
        },
        &text,
        &msg.message_id.to_string(),
        mentions,
    );
    event.attachments = attachments;

    let json = serde_json::to_string(&event).unwrap();
    info!(chat_id = %msg.chat.id, sender = %sender_name, "telegram → gateway");
    let _ = state.event_tx.send(json);
    axum::http::StatusCode::OK
}

// --- Reply handler ---

pub async fn handle_reply(
    reply: &GatewayReply,
    bot_token: &str,
    client: &reqwest::Client,
    event_tx: &tokio::sync::broadcast::Sender<String>,
    reaction_state: &Arc<Mutex<HashMap<String, Vec<String>>>>,
) {
    // Handle create_topic command
    if reply.command.as_deref() == Some("create_topic") {
        let req_id = reply.request_id.clone().unwrap_or_default();
        info!(chat_id = %reply.channel.id, "creating forum topic");
        let url = format!("{TELEGRAM_API_BASE}/bot{bot_token}/createForumTopic");
        let resp = client
            .post(&url)
            .json(&serde_json::json!({"chat_id": reply.channel.id, "name": reply.content.text}))
            .send()
            .await;
        let gw_resp = match resp {
            Ok(r) => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
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
                        message_id: None,
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
                        message_id: None,
                        error: Some(err),
                    }
                }
            }
            Err(e) => GatewayResponse {
                schema: "openab.gateway.response.v1".into(),
                request_id: req_id,
                success: false,
                thread_id: None,
                message_id: None,
                error: Some(e.to_string()),
            },
        };
        let json = serde_json::to_string(&gw_resp).unwrap();
        let _ = event_tx.send(json);
        return;
    }

    // Handle add_reaction / remove_reaction
    if reply.command.as_deref() == Some("add_reaction")
        || reply.command.as_deref() == Some("remove_reaction")
    {
        let msg_key = format!("{}:{}", reply.channel.id, reply.reply_to);
        let emoji = &reply.content.text;
        let tg_emoji = match emoji.as_str() {
            "🆗" => "👍",
            other => other,
        };
        let is_add = reply.command.as_deref() == Some("add_reaction");
        {
            let mut reactions = reaction_state.lock().await;
            let set = reactions.entry(msg_key.clone()).or_default();
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
                        .map(|e| serde_json::json!({"type": "emoji", "emoji": e}))
                        .collect()
                })
                .unwrap_or_default()
        };
        let url = format!("{TELEGRAM_API_BASE}/bot{bot_token}/setMessageReaction");
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
        return;
    }

    // Normal send_message
    info!(
        chat_id = %reply.channel.id,
        thread_id = ?reply.channel.thread_id,
        "gateway → telegram"
    );
    let url = format!("{TELEGRAM_API_BASE}/bot{bot_token}/sendMessage");
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

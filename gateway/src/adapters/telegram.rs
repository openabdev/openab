use crate::media::{resize_and_compress, MediaKind, AUDIO_MAX_DOWNLOAD, FILE_MAX_DOWNLOAD, IMAGE_MAX_DOWNLOAD};
use crate::schema::*;
use crate::store;
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
    caption: Option<String>,
    #[serde(default)]
    entities: Vec<TelegramEntity>,
    #[serde(default)]
    caption_entities: Vec<TelegramEntity>,
    #[serde(default)]
    photo: Vec<TelegramPhoto>,
    document: Option<TelegramDocument>,
    voice: Option<TelegramVoice>,
    audio: Option<TelegramAudio>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhoto {
    file_id: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
    #[allow(dead_code)] // TODO: use for Content-Type hint
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramAudio {
    file_id: String,
    #[allow(dead_code)] // TODO: use for filename
    file_name: Option<String>,
    #[allow(dead_code)] // TODO: use for Content-Type hint
    mime_type: Option<String>,
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
    let is_photo = !msg.photo.is_empty();
    let is_document = msg.document.is_some();
    let is_voice = msg.voice.is_some();
    let is_audio = msg.audio.is_some();
    let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");

    if text.trim().is_empty() && !is_photo && !is_document && !is_voice && !is_audio {
        return axum::http::StatusCode::OK;
    }

    let mut attachments = Vec::new();
    if is_photo || is_document || is_voice || is_audio {
        if let Some(ref token) = state.telegram_bot_token {
            let client = &state.client;
            if is_photo {
                if let Some(largest) = msg.photo.iter().max_by_key(|p| p.width * p.height) {
                    if let Some(att) =
                        download_telegram_media(client, token, &largest.file_id, MediaKind::Image).await
                    {
                        attachments.push(att);
                    }
                }
            } else if let Some(doc) = msg.document {
                let file_name = doc.file_name.unwrap_or_else(|| "unknown.txt".to_string());
                let mime_type = doc.mime_type.unwrap_or_else(|| "text/plain".to_string());
                if let Some(att) =
                    download_telegram_document(client, token, &doc.file_id, &file_name, &mime_type).await
                {
                    attachments.push(att);
                }
            } else if let Some(voice) = msg.voice {
                if let Some(att) = download_telegram_media(client, token, &voice.file_id, MediaKind::Audio).await {
                    attachments.push(att);
                }
            } else if let Some(audio) = msg.audio {
                if let Some(att) = download_telegram_media(client, token, &audio.file_id, MediaKind::Audio).await {
                    attachments.push(att);
                }
            }
        }
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
        .chain(msg.caption_entities.iter())
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
        text,
        &msg.message_id.to_string(),
        mentions,
    );
    event.content.attachments = attachments;

    // Guard: skip empty events (no text + no attachments)
    if event.content.text.trim().is_empty() && event.content.attachments.is_empty() {
        return axum::http::StatusCode::OK;
    }

    let json = serde_json::to_string(&event).unwrap();
    info!(chat_id = %msg.chat.id, sender = %sender_name, "telegram → gateway");
    let _ = state.event_tx.send(json);
    axum::http::StatusCode::OK
}

fn is_markdown_parse_error(description: &str) -> bool {
    let desc_lower = description.to_lowercase();
    desc_lower.contains("can't find end")
        || desc_lower.contains("can't parse")
        || desc_lower.contains("parse entities")
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
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": reply.channel.id,
            "text": &reply.content.text,
            "message_thread_id": reply.channel.thread_id,
            "parse_mode": "Markdown",
        }))
        .send()
        .await;

    match resp {
        Ok(r) => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            if body["ok"].as_bool() != Some(true) {
                let desc = body["description"].as_str().unwrap_or("unknown error");
                if is_markdown_parse_error(desc) {
                    warn!("Markdown send failed: {desc}, retrying as plain text");
                    match client
                        .post(&url)
                        .json(&serde_json::json!({
                            "chat_id": reply.channel.id,
                            "text": &reply.content.text,
                            "message_thread_id": reply.channel.thread_id,
                        }))
                        .send()
                        .await
                    {
                        Ok(retry_r) => {
                            let retry_body: serde_json::Value =
                                retry_r.json().await.unwrap_or_default();
                            if retry_body["ok"].as_bool() != Some(true) {
                                error!(
                                    "telegram plain-text retry failed: {}",
                                    retry_body["description"]
                                        .as_str()
                                        .unwrap_or("unknown error")
                                );
                            }
                        }
                        Err(e) => error!("telegram plain-text send error: {e}"),
                    }
                } else {
                    error!("telegram send failed: {desc}");
                }
            }
        }
        Err(e) => error!("telegram send error: {e}"),
    }
}

/// Download media from Telegram via getFile → store to filesystem (colocate mode).
async fn download_telegram_media(
    client: &reqwest::Client,
    bot_token: &str,
    file_id: &str,
    kind: MediaKind,
) -> Option<Attachment> {
    let get_file_url = format!("{TELEGRAM_API_BASE}/bot{}/getFile", bot_token);
    let resp = client.get(&get_file_url).query(&[("file_id", file_id)]).send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    let file_path = body["result"]["file_path"].as_str()?;

    let download_url = format!("{TELEGRAM_API_BASE}/file/bot{}/{}", bot_token, file_path);
    let resp = client.get(&download_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let max_size = match kind {
        MediaKind::Image => IMAGE_MAX_DOWNLOAD,
        MediaKind::Audio => AUDIO_MAX_DOWNLOAD,
    };

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > max_size {
                warn!(file_id, size, kind = ?kind, "Telegram media Content-Length exceeds limit");
                return None;
            }
        }
    }

    let default_mime = match kind {
        MediaKind::Image => "image/jpeg",
        MediaKind::Audio => "audio/ogg",
    };
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or(default_mime)
        .to_string();

    let bytes = resp.bytes().await.ok()?;
    if bytes.len() as u64 > max_size {
        warn!(file_id, size = bytes.len(), kind = ?kind, "Telegram media exceeds limit");
        return None;
    }

    let (data_bytes, mime) = match kind {
        MediaKind::Image => match resize_and_compress(&bytes) {
            Ok((c, m)) => (c, m),
            Err(e) => {
                error!(err = %e, "Telegram image processing failed");
                return None;
            }
        },
        MediaKind::Audio => (bytes.to_vec(), content_type),
    };

    // Store to filesystem instead of base64 encoding
    let path = store::store_media(&data_bytes).await?;
    let att_type = match kind {
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
    };
    info!(file_id, size = data_bytes.len(), kind = ?kind, "Telegram media stored");

    Some(Attachment {
        attachment_type: att_type.into(),
        filename: format!("{}.{}", file_id, match kind {
            MediaKind::Image => "jpg",
            MediaKind::Audio => crate::media::audio_extension(&mime),
        }),
        mime_type: mime,
        data: String::new(), // No base64 — using file path
        size: data_bytes.len() as u64,
        path: Some(path),
    })
}

/// Download text document from Telegram → store to filesystem.
async fn download_telegram_document(
    client: &reqwest::Client,
    bot_token: &str,
    file_id: &str,
    file_name: &str,
    mime_type: &str,
) -> Option<Attachment> {
    if !crate::media::is_text_extension(file_name) {
        tracing::debug!(file_name, "skipping non-text file attachment");
        return None;
    }

    let get_file_url = format!("{TELEGRAM_API_BASE}/bot{}/getFile", bot_token);
    let resp = client.get(&get_file_url).query(&[("file_id", file_id)]).send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    let file_path = body["result"]["file_path"].as_str()?;

    let download_url = format!("{TELEGRAM_API_BASE}/file/bot{}/{}", bot_token, file_path);
    let resp = client.get(&download_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > FILE_MAX_DOWNLOAD {
                warn!(file_id, size, "Telegram document Content-Length exceeds limit");
                return None;
            }
        }
    }

    let bytes = resp.bytes().await.ok()?;
    if bytes.len() as u64 > FILE_MAX_DOWNLOAD {
        warn!(file_id, size = bytes.len(), "Telegram document exceeds limit");
        return None;
    }

    // Validate UTF-8 — reject binary files
    if String::from_utf8(bytes.to_vec()).is_err() {
        warn!(file_id, file_name, "Telegram document is not valid UTF-8, skipping");
        return None;
    }

    let path = store::store_media(&bytes).await?;
    info!(file_id, file_name, size = bytes.len(), "Telegram document stored");

    Some(Attachment {
        attachment_type: "text_file".into(),
        filename: file_name.to_string(),
        mime_type: mime_type.to_string(),
        data: String::new(),
        size: bytes.len() as u64,
        path: Some(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_markdown_parse_error() {
        assert!(is_markdown_parse_error("Bad Request: can't find end of italic entity at byte offset 37"));
        assert!(is_markdown_parse_error("Bad Request: can't parse entities: Can't find end of bold entity"));
        assert!(is_markdown_parse_error("can't parse entities in message text"));
        assert!(!is_markdown_parse_error("Unauthorized"));
        assert!(!is_markdown_parse_error("Bad Request: chat not found"));
    }
}

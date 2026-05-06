use crate::media::{resize_and_compress, FILE_MAX_DOWNLOAD, IMAGE_MAX_DOWNLOAD};
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
    caption: Option<String>,
    #[serde(default)]
    entities: Vec<TelegramEntity>,
    #[serde(default)]
    photo: Vec<TelegramPhoto>,
    document: Option<TelegramDocument>,
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
    let is_voice = msg.voice.is_some();
    let is_audio = msg.audio.is_some();
    let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");

    if text.trim().is_empty() && !is_photo && !is_document && !is_voice && !is_audio {
        return axum::http::StatusCode::OK;
    }

    let mut attachments = Vec::new();
    if is_photo || is_document || is_voice || is_audio {
        if let Some(ref token) = state.telegram_bot_token {
            let client = reqwest::Client::new();
            if is_photo {
                // Take the largest photo
                if let Some(largest) = msg.photo.iter().max_by_key(|p| p.width * p.height) {
                    if let Some(att) =
                        download_telegram_media(&client, token, &largest.file_id, "image").await
                    {
                        attachments.push(att);
                    }
                }
            } else if let Some(doc) = msg.document {
                let file_name = doc.file_name.unwrap_or_else(|| "unknown.txt".to_string());
                if let Some(att) =
                    download_telegram_document(&client, token, &doc.file_id, &file_name).await
                {
                    attachments.push(att);
                }
            } else if let Some(voice) = msg.voice {
                if let Some(att) = download_telegram_media(&client, token, &voice.file_id, "audio").await {
                    attachments.push(att);
                }
            } else if let Some(audio) = msg.audio {
                if let Some(att) = download_telegram_media(&client, token, &audio.file_id, "audio").await {
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

/// Download photo from Telegram via getFile + download URL.
async fn download_telegram_media(
    client: &reqwest::Client,
    bot_token: &str,
    file_id: &str,
    attachment_type: &str,
) -> Option<Attachment> {
    // 1. Get file path
    let get_file_url = format!("{TELEGRAM_API_BASE}/bot{}/getFile", bot_token);
    let resp = client
        .get(get_file_url)
        .query(&[("file_id", file_id)])
        .send()
        .await
        .ok()?;

    let body: serde_json::Value = resp.json().await.ok()?;
    let file_path = body["result"]["file_path"].as_str()?;

    // 2. Download file
    let download_url = format!("{TELEGRAM_API_BASE}/file/bot{}/{}", bot_token, file_path);
    let resp = client.get(download_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let max_size = if attachment_type == "image" {
        IMAGE_MAX_DOWNLOAD
    } else {
        AUDIO_MAX_DOWNLOAD
    };

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > max_size {
                warn!(file_id, size, "Telegram {} Content-Length exceeds limit", attachment_type);
                return None;
            }
        }
    }

    let bytes = resp.bytes().await.ok()?;
    if bytes.len() as u64 > max_size {
        warn!(file_id, size = bytes.len(), "Telegram {} exceeds limit", attachment_type);
        return None;
    }

    let (data_bytes, mime, filename) = if attachment_type == "image" {
        match resize_and_compress(&bytes) {
            Ok((c, m)) => (c, m, format!("{}.jpg", file_id)),
            Err(e) => {
                error!(err = %e, "Telegram image processing failed");
                return None;
            }
        }
    } else {
        // For audio/voice, we don't process.
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("audio/ogg") // Default for Telegram voice
            .to_string();
        let ext = if mime.contains("mpeg") || mime.contains("mp3") {
            "mp3"
        } else if mime.contains("m4a") {
            "m4a"
        } else {
            "ogg"
        };
        (bytes.to_vec(), mime, format!("{}.{}", file_id, ext))
    };

    use base64::Engine;
    let b64_data = base64::engine::general_purpose::STANDARD.encode(&data_bytes);

    Some(Attachment {
        attachment_type: attachment_type.into(),
        filename,
        mime_type: mime,
        data: b64_data,
        size: data_bytes.len() as u64,
    })
}

/// Download document from Telegram via getFile + download URL (text files only).
async fn download_telegram_document(
    client: &reqwest::Client,
    bot_token: &str,
    file_id: &str,
    file_name: &str,
) -> Option<Attachment> {
    // Only download text-like files
    let ext = file_name.rsplit('.').next().unwrap_or("").to_lowercase();
    const TEXT_EXTS: &[&str] = &[
        "txt", "csv", "log", "md", "json", "jsonl", "yaml", "yml", "toml", "xml", "rs", "py", "js",
        "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp", "rb", "sh", "bash", "sql",
        "html", "css", "ini", "cfg", "conf", "env",
    ];
    if !TEXT_EXTS.contains(&ext.as_str()) {
        tracing::debug!(file_name, "skipping non-text file attachment");
        return None;
    }

    // 1. Get file path
    let get_file_url = format!("{TELEGRAM_API_BASE}/bot{}/getFile", bot_token);
    let resp = client
        .get(get_file_url)
        .query(&[("file_id", file_id)])
        .send()
        .await
        .ok()?;

    let body: serde_json::Value = resp.json().await.ok()?;
    let file_path = body["result"]["file_path"].as_str()?;

    // 2. Download file
    let download_url = format!("{TELEGRAM_API_BASE}/file/bot{}/{}", bot_token, file_path);
    let resp = client.get(download_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > FILE_MAX_DOWNLOAD {
                warn!(
                    file_id,
                    size, "Telegram document Content-Length exceeds limit"
                );
                return None;
            }
        }
    }

    let bytes = resp.bytes().await.ok()?;
    if bytes.len() as u64 > FILE_MAX_DOWNLOAD {
        warn!(
            file_id,
            size = bytes.len(),
            "Telegram document exceeds limit"
        );
        return None;
    }

    let text = String::from_utf8_lossy(&bytes);
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());

    Some(Attachment {
        attachment_type: "text_file".into(),
        filename: file_name.to_string(),
        mime_type: "text/plain".into(),
        data,
        size: bytes.len() as u64,
    })
}

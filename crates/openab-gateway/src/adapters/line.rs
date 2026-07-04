use crate::media::{
    audio_extension, format_bytes, resize_and_compress, AUDIO_MAX_DOWNLOAD, IMAGE_MAX_DOWNLOAD,
};
use crate::schema::*;
use crate::store;
use axum::extract::State;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

// --- LINE types ---

#[derive(Debug, Deserialize)]
pub struct LineWebhookBody {
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
    #[serde(rename = "contentProvider")]
    content_provider: Option<LineContentProvider>,
    mention: Option<LineMention>,
}

#[derive(Debug, Deserialize)]
struct LineMention {
    mentionees: Vec<LineMentionee>,
}

#[derive(Debug, Deserialize)]
struct LineMentionee {
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "isSelf", default)]
    is_self: bool,
}

#[derive(Debug, Deserialize)]
struct LineContentProvider {
    #[serde(rename = "type")]
    provider_type: String,
    #[serde(rename = "originalContentUrl")]
    original_content_url: Option<String>,
}

/// Base URL for LINE Messaging API. Overridden in tests via the `api_base` parameter.
pub const LINE_API_BASE: &str = "https://api.line.me";
/// Base URL for LINE binary content download API.
pub const LINE_DATA_API_BASE: &str = "https://api-data.line.me";

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
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

    let webhook_received_at = std::time::Instant::now();
    let background_state = state.clone();
    let permit = match background_state
        .line_webhook_semaphore
        .clone()
        .acquire_owned()
        .await
    {
        Ok(permit) => permit,
        Err(_) => {
            warn!("LINE webhook worker semaphore closed unexpectedly");
            return axum::http::StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    tokio::spawn(async move {
        let _permit = permit;
        process_line_webhook_events(background_state, webhook_body, webhook_received_at).await;
    });

    axum::http::StatusCode::OK
}

async fn process_line_webhook_events(
    state: Arc<crate::AppState>,
    webhook_body: LineWebhookBody,
    webhook_received_at: std::time::Instant,
) {
    // Acknowledge the webhook before image download/processing so LINE does not
    // redeliver solely because gateway-side attachment work is slow. We keep one
    // task per webhook payload so events from the same payload preserve order.
    //
    // Tradeoff:
    // - Pros: lowers webhook latency and reduces redelivery pressure from LINE.
    // - Cons: once 200 OK is returned, a later crash/task failure will not be
    //   retried by LINE. This PR intentionally keeps scope small and does not add
    //   background-task durability or duplicate suppression on top of early-ack.
    // - Cons: an earlier image event from one webhook payload can also be emitted
    //   after a later text event from another payload if the image path is slower.
    // - Guardrail: a shared semaphore bounds how many LINE payloads can enter the
    //   post-ack path concurrently. When saturated, new webhooks wait for capacity
    //   before spawning background work so bursts do not create unbounded backlog.
    for event in webhook_body.events {
        let Some(gateway_event) = build_gateway_event_from_line_event(
            &event,
            &state.client,
            state.line_access_token.as_deref(),
            LINE_DATA_API_BASE,
        )
        .await
        else {
            continue;
        };

        // Cache before broadcasting the event. Once event_tx.send() fires, OAB
        // may reply immediately; inserting afterward can silently force Push API.
        // We still use webhook receipt time so TTL reflects true reply-token age.
        if let Some(ref reply_token) = event.reply_token {
            let mut cache = state
                .reply_token_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cache.len() >= crate::REPLY_TOKEN_CACHE_MAX {
                warn!(
                    size = cache.len(),
                    "reply token cache full, skipping insert"
                );
            } else {
                cache.insert(
                    gateway_event.event_id.clone(),
                    (reply_token.clone(), webhook_received_at),
                );
                info!(event_id = %gateway_event.event_id, "cached LINE replyToken");
            }
        }

        let json = serde_json::to_string(&gateway_event).unwrap();
        info!(channel = %gateway_event.channel.id, sender = %gateway_event.sender.id, "line → gateway");
        let _ = state.event_tx.send(json);
    }
}

fn sanitize_line_external_url_for_log(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| "invalid-or-missing-host".to_string())
}

async fn build_gateway_event_from_line_event(
    event: &LineEvent,
    client: &reqwest::Client,
    line_access_token: Option<&str>,
    data_api_base: &str,
) -> Option<GatewayEvent> {
    if event.event_type != "message" {
        return None;
    }

    let msg = event.message.as_ref()?;
    if msg.message_type != "text" && msg.message_type != "image" && msg.message_type != "audio" {
        return None;
    }

    let text = msg.text.as_deref().unwrap_or("");
    let mut attachments = Vec::new();

    if msg.message_type == "image" {
        match msg
            .content_provider
            .as_ref()
            .map(|provider| provider.provider_type.as_str())
        {
            Some("external") => {
                let original = msg
                    .content_provider
                    .as_ref()
                    .and_then(|provider| provider.original_content_url.as_deref())
                    .unwrap_or("unknown");
                warn!(
                    message_id = %msg.id,
                    external_content_host = %sanitize_line_external_url_for_log(original),
                    "LINE external image content is not supported yet"
                );
                attachments.push(Attachment {
                    attachment_type: "image".into(),
                    filename: format!("line_{}.jpg", msg.id),
                    mime_type: "image/jpeg".into(),
                    data: String::new(),
                    size: 0,
                    path: None,
                    status: Some("unsupported format: external content not supported".into()),
                });
            }
            _ => {
                if let Some(access_token) = line_access_token {
                    attachments.push(
                        download_line_image(client, access_token, &msg.id, data_api_base).await,
                    );
                } else {
                    warn!(message_id = %msg.id, "LINE image received but LINE_CHANNEL_ACCESS_TOKEN is not configured");
                    attachments.push(Attachment {
                        attachment_type: "image".into(),
                        filename: format!("line_{}.jpg", msg.id),
                        mime_type: "image/jpeg".into(),
                        data: String::new(),
                        size: 0,
                        path: None,
                        status: Some("configuration error: service not configured".into()),
                    });
                }
            }
        }
    }

    if msg.message_type == "audio" {
        match msg
            .content_provider
            .as_ref()
            .map(|provider| provider.provider_type.as_str())
        {
            Some("external") => {
                let original = msg
                    .content_provider
                    .as_ref()
                    .and_then(|provider| provider.original_content_url.as_deref())
                    .unwrap_or("unknown");
                warn!(
                    message_id = %msg.id,
                    external_content_host = %sanitize_line_external_url_for_log(original),
                    "LINE external audio content is not supported yet"
                );
                attachments.push(Attachment {
                    attachment_type: "audio".into(),
                    filename: format!("line_{}.audio", msg.id),
                    mime_type: "audio/ogg".into(),
                    data: String::new(),
                    size: 0,
                    path: None,
                    status: Some("unsupported format: external content not supported".into()),
                });
            }
            _ => {
                if let Some(access_token) = line_access_token {
                    attachments.push(
                        download_line_audio(client, access_token, &msg.id, data_api_base).await,
                    );
                } else {
                    warn!(message_id = %msg.id, "LINE audio received but LINE_CHANNEL_ACCESS_TOKEN is not configured");
                    attachments.push(Attachment {
                        attachment_type: "audio".into(),
                        filename: format!("line_{}.audio", msg.id),
                        mime_type: "audio/ogg".into(),
                        data: String::new(),
                        size: 0,
                        path: None,
                        status: Some("configuration error: service not configured".into()),
                    });
                }
            }
        }
    }

    let event_text = text;

    if event_text.trim().is_empty() && attachments.is_empty() {
        return None;
    }

    let source = event.source.as_ref();
    let (channel_id, channel_type) = match source {
        Some(s) if s.source_type == "group" => match s.group_id.as_deref() {
            Some(id) if !id.is_empty() => (id.to_string(), "group".to_string()),
            _ => {
                warn!("LINE group event missing groupId, skipping");
                return None;
            }
        },
        Some(s) if s.source_type == "room" => match s.room_id.as_deref() {
            Some(id) if !id.is_empty() => (id.to_string(), "room".to_string()),
            _ => {
                warn!("LINE room event missing roomId, skipping");
                return None;
            }
        },
        Some(s) => match s.user_id.as_deref() {
            Some(id) if !id.is_empty() => (id.to_string(), "user".to_string()),
            _ => {
                warn!("LINE user event missing userId, skipping");
                return None;
            }
        },
        None => {
            warn!("LINE event missing source, skipping");
            return None;
        }
    };
    let user_id = source
        .and_then(|s| s.user_id.as_deref())
        .unwrap_or("unknown");

    // Extract mentioned user IDs from the LINE webhook mention object.
    // LINE populates this in group/room text messages when users are @-mentioned.
    let mentionees = msg
        .mention
        .as_ref()
        .map(|m| m.mentionees.as_slice())
        .unwrap_or_default();
    let mention_ids: Vec<String> = mentionees
        .iter()
        .filter_map(|m| m.user_id.clone())
        .collect();

    // @mention gating: in groups/rooms, only forward the event if the bot is mentioned.
    // LINE sets isSelf=true on the mentionee that is the bot itself — no env var needed.
    // 1:1 DMs always pass through.
    let is_group = channel_type == "group" || channel_type == "room";
    if is_group && !mentionees.iter().any(|m| m.is_self) {
        info!(
            channel = %channel_id,
            "line group message dropped (@mention gating: bot not mentioned)"
        );
        return None;
    }

    let mut gateway_event = GatewayEvent::new(
        "line",
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
        event_text,
        &msg.id,
        mention_ids,
    );
    gateway_event.content.attachments = attachments;
    Some(gateway_event)
}

pub async fn download_line_image(
    client: &reqwest::Client,
    access_token: &str,
    message_id: &str,
    api_base: &str,
) -> Attachment {
    let rejected = |size: u64, reason: String| Attachment {
        attachment_type: "image".into(),
        filename: format!("line_{}.jpg", message_id),
        mime_type: "image/jpeg".into(),
        data: String::new(),
        size,
        path: None,
        status: Some(reason),
    };

    let mut resp = match client
        .get(format!(
            "{}/v2/bot/message/{}/content",
            api_base, message_id
        ))
        .bearer_auth(access_token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            warn!(message_id, error = %e, "LINE image download failed");
            return rejected(0, "download failed: network error".into());
        }
    };

    if !resp.status().is_success() {
        let http_status = resp.status().as_u16();
        warn!(message_id, status = %resp.status(), "LINE image download failed");
        return rejected(0, format!("download failed: HTTP {http_status}"));
    }

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > IMAGE_MAX_DOWNLOAD {
                warn!(message_id, size, "LINE image Content-Length exceeds limit");
                return rejected(
                    size,
                    format!(
                        "size exceeded: {} exceeds {}",
                        format_bytes(size),
                        format_bytes(IMAGE_MAX_DOWNLOAD)
                    ),
                );
            }
        }
    }

    let mut body = Vec::new();
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                warn!(message_id, error = %e, "LINE image download failed while reading body");
                return rejected(0, "download failed: body read error".into());
            }
        };
        body.extend_from_slice(&chunk);
        if body.len() as u64 > IMAGE_MAX_DOWNLOAD {
            let body_size = body.len() as u64;
            warn!(message_id, size = body_size, "LINE image exceeds limit");
            return rejected(
                body_size,
                format!(
                    "size exceeded: {} exceeds {}",
                    format_bytes(body_size),
                    format_bytes(IMAGE_MAX_DOWNLOAD)
                ),
            );
        }
    }

    let (compressed, mime) =
        match tokio::task::spawn_blocking(move || resize_and_compress(&body)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                warn!(message_id, error = %e, "LINE image resize/compress failed");
                return rejected(0, "processing failed: image encoding error".into());
            }
            Err(e) => {
                warn!(message_id, error = %e, "LINE image processing task failed");
                return rejected(0, "processing failed: image encoding error".into());
            }
        };
    let path = match store::store_media(&compressed).await {
        Some(p) => p,
        None => {
            warn!(message_id, "LINE image store failed");
            return rejected(0, "processing failed: storage error".into());
        }
    };
    let ext = if mime == "image/gif" { "gif" } else { "jpg" };
    Attachment {
        attachment_type: "image".into(),
        filename: format!("line_{}.{}", message_id, ext),
        mime_type: mime,
        data: String::new(),
        size: compressed.len() as u64,
        path: Some(path),
        status: None,
    }
}

pub async fn download_line_audio(
    client: &reqwest::Client,
    access_token: &str,
    message_id: &str,
    api_base: &str,
) -> Attachment {
    let rejected = |filename: String, mime_type: String, size: u64, reason: String| Attachment {
        attachment_type: "audio".into(),
        filename,
        mime_type,
        data: String::new(),
        size,
        path: None,
        status: Some(reason),
    };

    let mut resp = match client
        .get(format!(
            "{}/v2/bot/message/{}/content",
            api_base, message_id
        ))
        .bearer_auth(access_token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            warn!(message_id, error = %e, "LINE audio download failed");
            return rejected(
                format!("line_{}.audio", message_id),
                "audio/ogg".into(),
                0,
                "download failed: network error".into(),
            );
        }
    };

    if !resp.status().is_success() {
        let http_status = resp.status().as_u16();
        warn!(message_id, status = %resp.status(), "LINE audio download failed");
        return rejected(
            format!("line_{}.audio", message_id),
            "audio/ogg".into(),
            0,
            format!("download failed: HTTP {http_status}"),
        );
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("audio/ogg")
        .to_string();
    let filename = format!("line_{}.{}", message_id, audio_extension(&content_type));

    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > AUDIO_MAX_DOWNLOAD {
                warn!(message_id, size, "LINE audio Content-Length exceeds limit");
                return rejected(
                    filename.clone(),
                    content_type.clone(),
                    size,
                    format!(
                        "size exceeded: {} exceeds {}",
                        format_bytes(size),
                        format_bytes(AUDIO_MAX_DOWNLOAD)
                    ),
                );
            }
        }
    }

    let mut body = Vec::new();
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                warn!(message_id, error = %e, "LINE audio download failed while reading body");
                return rejected(
                    filename.clone(),
                    content_type.clone(),
                    0,
                    "download failed: body read error".into(),
                );
            }
        };
        body.extend_from_slice(&chunk);
        if body.len() as u64 > AUDIO_MAX_DOWNLOAD {
            let body_size = body.len() as u64;
            warn!(message_id, size = body_size, "LINE audio exceeds limit");
            return rejected(
                filename.clone(),
                content_type.clone(),
                body_size,
                format!(
                    "size exceeded: {} exceeds {}",
                    format_bytes(body_size),
                    format_bytes(AUDIO_MAX_DOWNLOAD)
                ),
            );
        }
    }

    let path = match store::store_media(&body).await {
        Some(p) => p,
        None => {
            warn!(message_id, "LINE audio store failed");
            return rejected(
                filename,
                content_type,
                body.len() as u64,
                "processing failed: storage error".into(),
            );
        }
    };

    Attachment {
        attachment_type: "audio".into(),
        filename,
        mime_type: content_type,
        data: String::new(),
        size: body.len() as u64,
        path: Some(path),
        status: None,
    }
}

// --- Reply handler (hybrid Reply/Push dispatch) ---

/// Dispatch a reply to LINE using the hybrid Reply/Push strategy.
///
/// Returns `true` if Reply API was used (or assumed used), `false` if Push API was used.
pub async fn dispatch_line_reply(
    client: &reqwest::Client,
    access_token: &str,
    reply_cache: &crate::ReplyTokenCache,
    reply: &GatewayReply,
    api_base: &str,
) -> bool {
    if matches!(
        reply.command.as_deref(),
        Some("add_reaction") | Some("remove_reaction") | Some("create_topic")
    ) {
        info!(command = ?reply.command.as_deref(), "line: ignoring unsupported command");
        return false;
    }

    // LINE's Messaging API cannot edit or delete a message once it is sent.
    // In streaming mode the core posts a placeholder then repeatedly calls
    // edit_message with the growing text; on an editable platform this updates in
    // place, but on LINE every edit_message would be delivered as a *new* message
    // — the reply gets reposted several times, each copy longer than the last.
    // Because the unified adapter uses a "draft" placeholder (no real message), the
    // final content is delivered separately via send_message, so dropping these
    // cosmetic edit/delete commands removes the duplicates without losing content.
    if matches!(
        reply.command.as_deref(),
        Some("edit_message") | Some("delete_message")
    ) {
        info!(command = ?reply.command.as_deref(), "line: ignoring edit/delete command (LINE cannot edit messages)");
        return false;
    }

    // Extract token from cache (drop lock before HTTP call)
    let cached_token = {
        let mut cache = reply_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .remove(&reply.reply_to)
            .and_then(|(token, cached_at)| {
                if cached_at.elapsed().as_secs() < crate::REPLY_TOKEN_TTL_SECS {
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
            .post(format!("{}/v2/bot/message/reply", api_base))
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
                let body_lower = body.to_lowercase();
                let token_unusable = status.as_u16() == 400
                    && ((body_lower.contains("invalid") && body_lower.contains("reply token"))
                        || body_lower.contains("expired"));
                if token_unusable {
                    warn!(status = %status, body = %body, "LINE reply token unusable, falling back to Push");
                } else {
                    error!(status = %status, body = %body, "LINE Reply API error, NOT falling back to Push (possible duplicate risk)");
                    used_reply = true;
                }
            }
            Err(e) => {
                error!(err = %e, "LINE Reply API network error, NOT falling back to Push (possible duplicate risk)");
                used_reply = true;
            }
        }
    }

    // Fallback to Push API
    if !used_reply {
        info!(to = %reply.channel.id, "gateway → line (push API)");
        let _ = client
            .post(format!("{}/v2/bot/message/push", api_base))
            .bearer_auth(access_token)
            .json(&serde_json::json!({
                "to": reply.channel.id,
                "messages": [{"type": "text", "text": reply.content.text}]
            }))
            .send()
            .await
            .map_err(|e| error!("line push error: {e}"));
    }

    used_reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn download_line_image_resizes_and_returns_attachment() {
        let server = MockServer::start().await;
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([0, 128, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let bytes = buf.into_inner();

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg123/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(bytes),
            )
            .mount_as_scoped(&server)
            .await;

        let attachment = download_line_image(
            &reqwest::Client::new(),
            "line_token",
            "msg123",
            &server.uri(),
        )
        .await;

        assert_eq!(attachment.attachment_type, "image");
        assert!(attachment.filename.starts_with("line_msg123."));
        assert!(attachment.path.is_some());
        assert!(attachment.size > 0);
        assert!(attachment.status.is_none());

        let path = attachment.path.unwrap();
        let stored = tokio::fs::read(&path).await.unwrap();
        assert!(!stored.is_empty());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn build_gateway_event_from_line_image_attaches_downloaded_image() {
        let server = MockServer::start().await;
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let bytes = buf.into_inner();

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg_image/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(bytes),
            )
            .mount_as_scoped(&server)
            .await;

        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "replyToken": "reply123",
            "source": {"type": "user", "userId": "U123"},
            "message": {
                "id": "msg_image",
                "type": "image",
                "contentProvider": {"type": "line"}
            }
        }))
        .unwrap();

        let gateway_event = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            Some("line_token"),
            &server.uri(),
        )
        .await
        .expect("image event should produce a gateway event");

        assert_eq!(gateway_event.platform, "line");
        assert_eq!(gateway_event.content.text, "");
        assert_eq!(gateway_event.content.attachments.len(), 1);

        let att = &gateway_event.content.attachments[0];
        assert!(
            att.status.is_none(),
            "successful download should have no status"
        );
        let path = att.path.clone().expect("path should be stored");
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn build_gateway_event_from_line_audio_attaches_downloaded_audio() {
        let server = MockServer::start().await;
        let audio_bytes = b"OggS-test-audio".to_vec();

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg_audio/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/ogg")
                    .set_body_bytes(audio_bytes),
            )
            .mount_as_scoped(&server)
            .await;

        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "replyToken": "reply123",
            "source": {"type": "user", "userId": "U123"},
            "message": {
                "id": "msg_audio",
                "type": "audio",
                "contentProvider": {"type": "line"}
            }
        }))
        .unwrap();

        let gateway_event = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            Some("line_token"),
            &server.uri(),
        )
        .await
        .expect("audio event should produce a gateway event");

        assert_eq!(gateway_event.platform, "line");
        assert_eq!(gateway_event.content.text, "");
        assert_eq!(gateway_event.content.attachments.len(), 1);

        let att = &gateway_event.content.attachments[0];
        assert_eq!(att.attachment_type, "audio");
        assert_eq!(att.mime_type, "audio/ogg");
        assert!(att.filename.ends_with(".ogg"));
        assert!(
            att.status.is_none(),
            "successful download should have no status"
        );
        let path = att.path.clone().expect("path should be stored");
        let stored = tokio::fs::read(&path).await.unwrap();
        assert_eq!(stored, b"OggS-test-audio");
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn build_gateway_event_from_line_audio_mp4_uses_m4a_filename() {
        let server = MockServer::start().await;
        let audio_bytes = b"ftypM4A-test-audio".to_vec();

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg_audio_m4a/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/mp4")
                    .set_body_bytes(audio_bytes),
            )
            .mount_as_scoped(&server)
            .await;

        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "replyToken": "reply123",
            "source": {"type": "user", "userId": "U123"},
            "message": {
                "id": "msg_audio_m4a",
                "type": "audio",
                "contentProvider": {"type": "line"}
            }
        }))
        .unwrap();

        let gateway_event = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            Some("line_token"),
            &server.uri(),
        )
        .await
        .expect("audio event should produce a gateway event");

        let att = &gateway_event.content.attachments[0];
        assert_eq!(att.attachment_type, "audio");
        assert_eq!(att.mime_type, "audio/mp4");
        assert!(att.filename.ends_with(".m4a"));
        assert!(
            att.status.is_none(),
            "successful download should have no status"
        );
        let path = att.path.clone().expect("path should be stored");
        let stored = tokio::fs::read(&path).await.unwrap();
        assert_eq!(stored, b"ftypM4A-test-audio");
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn download_line_audio_rejects_oversized_content_length() {
        let server = MockServer::start().await;

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg_audio_big/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/ogg")
                    .insert_header("content-length", (AUDIO_MAX_DOWNLOAD + 1).to_string())
                    .set_body_bytes(vec![0u8; AUDIO_MAX_DOWNLOAD as usize + 1]),
            )
            .mount_as_scoped(&server)
            .await;

        let attachment = download_line_audio(
            &reqwest::Client::new(),
            "line_token",
            "msg_audio_big",
            &server.uri(),
        )
        .await;

        assert_eq!(attachment.attachment_type, "audio");
        assert!(attachment.path.is_none());
        assert!(attachment.status.is_some());
        let reason = attachment.status.unwrap();
        assert!(
            reason.contains("size exceeded"),
            "expected size exceeded reason, got: {reason}"
        );
        assert!(
            reason.contains("exceeds"),
            "expected size limit message, got: {reason}"
        );
    }

    #[tokio::test]
    async fn download_line_image_rejects_oversized_content_length() {
        let server = MockServer::start().await;

        let _mock = Mock::given(method("GET"))
            .and(path("/v2/bot/message/msg_big/content"))
            .and(header("authorization", "Bearer line_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .insert_header("content-length", (IMAGE_MAX_DOWNLOAD + 1).to_string())
                    .set_body_bytes(vec![0u8; IMAGE_MAX_DOWNLOAD as usize + 1]),
            )
            .mount_as_scoped(&server)
            .await;

        let attachment = download_line_image(
            &reqwest::Client::new(),
            "line_token",
            "msg_big",
            &server.uri(),
        )
        .await;

        assert!(attachment.status.is_some());
        let reason = attachment.status.unwrap();
        assert!(
            reason.contains("size exceeded"),
            "expected size exceeded reason, got: {reason}"
        );
        assert!(
            reason.contains("exceeds"),
            "expected size limit message, got: {reason}"
        );
    }

    #[tokio::test]
    async fn external_image_produces_status_attachment_not_dropped() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "user", "userId": "U_human"},
            "message": {
                "id": "msg_ext",
                "type": "image",
                "contentProvider": {
                    "type": "external",
                    "originalContentUrl": "https://example.com/photo.jpg"
                }
            }
        }))
        .unwrap();

        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;

        let gw = result.expect("external image event should not be dropped");
        assert_eq!(gw.content.attachments.len(), 1);
        let att = &gw.content.attachments[0];
        assert!(
            att.status.is_some(),
            "external image should have status set"
        );
        let reason = att.status.as_deref().unwrap();
        assert!(reason.contains("unsupported format"), "got: {reason}");
        assert!(reason.contains("external"), "got: {reason}");
    }

    #[tokio::test]
    async fn external_audio_produces_status_attachment_not_dropped() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "user", "userId": "U_human"},
            "message": {
                "id": "msg_audio_ext",
                "type": "audio",
                "contentProvider": {
                    "type": "external",
                    "originalContentUrl": "https://example.com/voice.ogg"
                }
            }
        }))
        .unwrap();

        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;

        let gw = result.expect("external audio event should not be dropped");
        assert_eq!(gw.content.attachments.len(), 1);
        let att = &gw.content.attachments[0];
        assert_eq!(att.attachment_type, "audio");
        assert!(
            att.status.is_some(),
            "external audio should have status set"
        );
        let reason = att.status.as_deref().unwrap();
        assert!(reason.contains("unsupported format"), "got: {reason}");
        assert!(reason.contains("external"), "got: {reason}");
    }

    #[tokio::test]
    async fn missing_access_token_produces_status_attachment_not_dropped() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "user", "userId": "U_human"},
            "message": {
                "id": "msg_notoken",
                "type": "image",
                "contentProvider": {"type": "line"}
            }
        }))
        .unwrap();

        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None, // no access token
            LINE_DATA_API_BASE,
        )
        .await;

        let gw = result.expect("image event with missing token should not be dropped");
        assert_eq!(gw.content.attachments.len(), 1);
        let att = &gw.content.attachments[0];
        assert!(att.status.is_some(), "missing token should have status set");
        let reason = att.status.as_deref().unwrap();
        assert!(reason.contains("configuration error"), "got: {reason}");
    }

    #[tokio::test]
    async fn missing_access_token_for_audio_produces_status_attachment_not_dropped() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "user", "userId": "U_human"},
            "message": {
                "id": "msg_audio_notoken",
                "type": "audio",
                "contentProvider": {"type": "line"}
            }
        }))
        .unwrap();

        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;

        let gw = result.expect("audio event with missing token should not be dropped");
        assert_eq!(gw.content.attachments.len(), 1);
        let att = &gw.content.attachments[0];
        assert_eq!(att.attachment_type, "audio");
        assert!(att.status.is_some(), "missing token should have status set");
        let reason = att.status.as_deref().unwrap();
        assert!(reason.contains("configuration error"), "got: {reason}");
    }

    #[tokio::test]
    async fn webhook_acknowledges_before_async_event_forwarding() {
        let (event_tx, mut event_rx) = broadcast::channel::<String>(8);
        let state = Arc::new(crate::AppState::test_default(event_tx));

        let body = axum::body::Bytes::from(
            serde_json::json!({
                "events": [{
                    "type": "message",
                    "replyToken": "reply123",
                    "source": {"type": "user", "userId": "U123"},
                    "message": {"id": "msg123", "type": "text", "text": "hello"}
                }]
            })
            .to_string(),
        );

        let status = webhook(State(state.clone()), axum::http::HeaderMap::new(), body).await;
        assert_eq!(status, axum::http::StatusCode::OK);

        let event_json = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("background task should forward an event")
            .expect("broadcast should succeed");
        let event: GatewayEvent = serde_json::from_str(&event_json).expect("valid gateway event");

        assert_eq!(event.message_id, "msg123");
        assert_eq!(event.content.text, "hello");

        let cache = state
            .reply_token_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (token, cached_at) = cache
            .get(&event.event_id)
            .expect("reply token should be cached");
        assert_eq!(token, "reply123");
        assert!(cached_at.elapsed() < std::time::Duration::from_secs(1));
    }

    // --- @mention gating tests ---

    fn make_group_text_event(text: &str, bot_mentioned: bool) -> LineEvent {
        let mention = if bot_mentioned {
            serde_json::json!({"mentionees": [{"userId": "Ubot123", "type": "user", "isSelf": true}]})
        } else {
            serde_json::json!({"mentionees": [{"userId": "Uother", "type": "user", "isSelf": false}]})
        };
        serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "group", "groupId": "C001", "userId": "U_sender"},
            "message": {
                "id": "msg001",
                "type": "text",
                "text": text,
                "mention": mention
            }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn group_message_passes_when_bot_mentioned() {
        let event = make_group_text_event("@Bot hello", true);
        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;
        assert!(result.is_some());
        let gw = result.unwrap();
        assert_eq!(gw.mentions, vec!["Ubot123"]);
    }

    #[tokio::test]
    async fn group_message_dropped_when_bot_not_mentioned() {
        let event = make_group_text_event("hey everyone", false);
        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn group_message_dropped_when_no_mention_at_all() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "group", "groupId": "C001", "userId": "U_sender"},
            "message": {"id": "msg001", "type": "text", "text": "plain message no mention"}
        }))
        .unwrap();
        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn dm_passes_even_without_mention() {
        let event: LineEvent = serde_json::from_value(serde_json::json!({
            "type": "message",
            "source": {"type": "user", "userId": "U_human"},
            "message": {"id": "msg002", "type": "text", "text": "hello bot"}
        }))
        .unwrap();
        let result = build_gateway_event_from_line_event(
            &event,
            &reqwest::Client::new(),
            None,
            LINE_DATA_API_BASE,
        )
        .await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn edit_message_command_is_ignored_not_forwarded() {
        // LINE cannot edit messages. A streaming `edit_message` reply must be
        // dropped, never forwarded as a new Reply/Push message — otherwise each
        // edit posts the growing text as a separate message (duplicate replies).
        let server = MockServer::start().await;
        // If the guard fails and the command falls through, the empty reply-token
        // cache forces the Push API. This expectation forbids that call.
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let cache: crate::ReplyTokenCache =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "evt1".into(),
            platform: "line".into(),
            channel: ReplyChannel {
                id: "U123".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                text: "partial streamed text".into(),
                attachments: vec![],
            },
            command: Some("edit_message".into()),
            request_id: None,
            quote_message_id: None,
        };

        let used_reply = dispatch_line_reply(
            &reqwest::Client::new(),
            "line_token",
            &cache,
            &reply,
            &server.uri(),
        )
        .await;

        assert!(!used_reply, "edit_message must not use the Reply API");
        // `_push` expect(0) is verified on drop: no push request was sent.
    }
}

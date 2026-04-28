use crate::schema::*;
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
}

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

        let gateway_event = GatewayEvent::new(
            "line",
            ChannelInfo {
                id: channel_id.clone(),
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
            &msg.id,
            vec![],
        );

        let json = serde_json::to_string(&gateway_event).unwrap();
        info!(channel = %channel_id, sender = %user_id, "line → gateway");
        let _ = state.event_tx.send(json);
    }

    axum::http::StatusCode::OK
}

// --- Reply handler ---

pub async fn handle_reply(reply: &GatewayReply, access_token: &str, client: &reqwest::Client) {
    info!(to = %reply.channel.id, "gateway → line");
    let _ = client
        .post("https://api.line.me/v2/bot/message/push")
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "to": reply.channel.id,
            "messages": [{"type": "text", "text": reply.content.text}]
        }))
        .send()
        .await
        .map_err(|e| error!("line send error: {e}"));
}

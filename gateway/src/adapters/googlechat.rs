use crate::schema::*;
use axum::extract::State;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};

pub const GOOGLE_CHAT_API_BASE: &str = "https://chat.googleapis.com/v1";

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
    pub space_type_renamed: Option<String>,
}

// --- Webhook handler ---

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    info!("googlechat webhook received ({} bytes)", body.len());

    let envelope: GoogleChatEnvelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            let body_str = String::from_utf8_lossy(&body);
            error!(body = %body_str, "googlechat webhook parse error: {e}");
            return axum::http::StatusCode::BAD_REQUEST;
        }
    };

    let Some(chat) = envelope.chat else {
        return axum::http::StatusCode::OK;
    };
    let Some(payload) = chat.message_payload else {
        return axum::http::StatusCode::OK;
    };
    let Some(ref msg) = payload.message else {
        return axum::http::StatusCode::OK;
    };

    let text = msg
        .argument_text
        .as_deref()
        .or(msg.text.as_deref())
        .unwrap_or("");
    if text.trim().is_empty() {
        return axum::http::StatusCode::OK;
    }

    let sender = msg.sender.as_ref().or(chat.user.as_ref());
    let space = msg.space.as_ref().or(payload.space.as_ref());

    let sender_id = sender.map(|s| s.name.clone()).unwrap_or_default();
    let display_name = sender
        .map(|s| s.display_name.clone())
        .unwrap_or_else(|| "Unknown".into());
    let sender_name = sender_id
        .strip_prefix("users/")
        .unwrap_or(&sender_id)
        .to_string();
    let is_bot = sender.map(|s| s.user_type == "BOT").unwrap_or(false);

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
            is_bot,
        },
        text,
        &message_id,
        vec![],
    );

    let json = serde_json::to_string(&gw_event).unwrap();
    info!(space = %space_name, sender = %sender_name, "googlechat → gateway");
    let _ = state.event_tx.send(json);
    axum::http::StatusCode::OK
}

// --- Reply handler ---

pub async fn handle_reply(
    reply: &GatewayReply,
    access_token: Option<&str>,
    client: &reqwest::Client,
) {
    if reply.command.as_deref() == Some("add_reaction")
        || reply.command.as_deref() == Some("remove_reaction")
    {
        return;
    }

    if reply.command.as_deref() == Some("create_topic") {
        return;
    }

    info!(
        space = %reply.channel.id,
        thread_id = ?reply.channel.thread_id,
        "gateway → googlechat"
    );

    let Some(token) = access_token else {
        info!(
            text = %reply.content.text,
            "googlechat reply (dry-run, no credentials configured)"
        );
        return;
    };

    let mut url = format!("{}/{}/messages", GOOGLE_CHAT_API_BASE, reply.channel.id);

    let mut body = serde_json::json!({
        "text": reply.content.text,
    });

    if let Some(ref thread_id) = reply.channel.thread_id {
        body["thread"] = serde_json::json!({
            "name": thread_id,
        });
        url.push_str("?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD");
    }

    let _ = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| error!("googlechat send error: {e}"));
}

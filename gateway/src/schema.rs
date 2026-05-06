use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(rename = "type")]
    pub attachment_type: String, // "image", "text_file"
    pub filename: String,
    pub mime_type: String,
    pub data: String, // base64 encoded
    pub size: u64,    // size in bytes (after compression for images)
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
    pub message_id: Option<String>,
    pub error: Option<String>,
}

impl GatewayEvent {
    pub fn new(
        platform: &str,
        channel: ChannelInfo,
        sender: SenderInfo,
        text: &str,
        message_id: &str,
        mentions: Vec<String>,
    ) -> Self {
        Self {
            schema: "openab.gateway.event.v1".into(),
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            platform: platform.into(),
            event_type: "message".into(),
            channel,
            sender,
            content: Content {
                content_type: "text".into(),
                text: text.into(),
                attachments: Vec::new(),
            },
            mentions,
            message_id: message_id.into(),
        }
    }
}

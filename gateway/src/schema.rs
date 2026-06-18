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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Content {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(rename = "type")]
    pub attachment_type: String, // "image", "text_file", "audio"
    pub filename: String,
    pub mime_type: String,
    /// Base64-encoded data (deprecated — use `path` for colocate mode).
    /// Kept for backward compatibility; Core prefers `path` when present.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    pub size: u64, // size in bytes (after compression for images)
    /// Local file path for colocate mode (gateway + core share filesystem).
    /// When set, Core reads bytes directly from this path instead of decoding `data`.
    /// Path format: ~/.openab/media/inbound/<uuid> (no extension, MIME in mime_type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Absent = attachment delivered normally (path/data available).
    /// Present = attachment could not be delivered; value is a human-readable reason.
    ///
    /// **Contract** — value format: `"<category>: <detail>"`.
    /// Category values and their meanings:
    ///   - `"size exceeded"` — file size exceeds the platform limit
    ///   - `"unsupported format"` — file type or content provider not supported
    ///   - `"download failed"` — attachment could not be retrieved
    ///   - `"processing failed"` — attachment retrieved but could not be processed
    ///   - `"configuration error"` — required service configuration is missing
    ///   - `"invalid content"` — content failed validation (e.g. encoding)
    ///   - `"security rejected"` — request blocked for security reasons
    ///
    /// When set, `data` and `path` are empty; `filename`, `mime_type`, and `size`
    /// (original file size, before processing) are preserved as metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl Attachment {
    /// Create a rejected attachment carrying a human-readable status reason.
    /// `size` should be the original file size in bytes (0 if unknown).
    pub fn rejected(
        attachment_type: &str,
        filename: impl Into<String>,
        mime_type: &str,
        size: u64,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            attachment_type: attachment_type.into(),
            filename: filename.into(),
            mime_type: mime_type.into(),
            data: String::new(),
            size,
            path: None,
            status: Some(reason.into()),
        }
    }
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
    /// When set, send this message as a reply/quote to the specified platform message ID.
    /// Unlike `reply_to` (which identifies the triggering event for routing/dedup),
    /// this field controls the visual reply/quote UI on the platform.
    /// If quoting fails, the gateway MUST fall back to sending without quoting.
    #[serde(default)]
    pub quote_message_id: Option<String>,
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

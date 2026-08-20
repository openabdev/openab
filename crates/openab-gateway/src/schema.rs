use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Authenticated platform scope used for trust decisions. Additive and
    /// optional so old Gateway/Core peers retain their legacy behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<GatewayScope>,
    /// Receiving bot identity, distinct from the human sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<RecipientInfo>,
    /// Structured mention entities. `mentions` remains the cross-platform ID
    /// list; this richer form lets Core remove only the receiving bot's text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mention_entities: Vec<MentionInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GatewayScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    pub conversation_type: String,
    pub trust_scope_id: String,
    pub is_dm: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecipientInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MentionInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
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
    /// Gateway-local opaque reference. Core may request materialization only
    /// after trust admission and only when the peer advertises support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
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
    pub fn decoded_data(&self) -> Result<Vec<u8>, base64::DecodeError> {
        base64::engine::general_purpose::STANDARD.decode(&self.data)
    }

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
            reference: None,
            data: String::new(),
            size,
            path: None,
            status: Some(reason.into()),
        }
    }
}

// --- Gateway protocol negotiation and capability schema ---

pub const CLIENT_HELLO_SCHEMA: &str = "openab.gateway.client_hello.v1";
pub const GATEWAY_HELLO_SCHEMA: &str = "openab.gateway.hello.v1";
pub const GATEWAY_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
pub struct GatewayEnvelope {
    pub schema: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingMode {
    #[default]
    Disabled,
    Edit,
    Native,
}

/// Conservative text budget from Microsoft's recommended 80 KB Teams
/// implementation target. Decimal bytes are intentional.
pub const TEAMS_TEXT_UTF16_BUDGET_BYTES: usize = 80_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum MessageLimit {
    Characters { max: usize },
    Bytes { max: usize },
    Utf16Bytes { max: usize },
    Unlimited,
}

impl Default for MessageLimit {
    fn default() -> Self {
        Self::Characters { max: 4096 }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusBackend {
    #[default]
    None,
    Reactions,
    Assistant,
    Typing,
    Message,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdapterCapabilities {
    pub send_ack: bool,
    pub edit_ack: bool,
    pub delete_ack: bool,
    /// Whether command targets use the additive `target_message_id` field.
    /// False peers require the legacy `reply_to = target` fallback.
    pub supports_target_message_id: bool,
    /// Native reactions may coexist with a different transient status backend.
    #[serde(default)]
    pub supports_reactions: bool,
    /// Resolve opaque inbound attachment references after Core admission.
    #[serde(default)]
    pub supports_attachment_materialization: bool,
    /// Persist an authenticated Teams route only after Core trust admission.
    #[serde(default)]
    pub supports_conversation_registry: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub streaming_mode: StreamingMode,
    pub show_streaming_placeholder: bool,
    pub message_limit: MessageLimit,
    pub status_backend: StatusBackend,
}

impl Default for AdapterCapabilities {
    fn default() -> Self {
        Self {
            send_ack: false,
            edit_ack: false,
            delete_ack: false,
            supports_target_message_id: false,
            supports_reactions: false,
            supports_attachment_materialization: false,
            supports_conversation_registry: false,
            can_edit: false,
            can_delete: false,
            streaming_mode: StreamingMode::Disabled,
            show_streaming_placeholder: true,
            message_limit: MessageLimit::default(),
            status_backend: StatusBackend::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayClientHello {
    pub schema: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub requested_platforms: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayHello {
    pub schema: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub capabilities: HashMap<String, AdapterCapabilities>,
    pub topology: GatewayTopology,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayTopology {
    pub active_consumers: usize,
    pub supported: bool,
    pub delivery_mode: String,
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
    /// Platform message targeted by a command such as edit or delete.
    /// `reply_to` remains the origin event correlation for peers that advertise
    /// support; old peers continue to place the command target in `reply_to`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_message_id: Option<String>,
    /// Opaque Gateway-local inbound attachment selected for materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_ref: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyChannel {
    pub id: String,
    pub thread_id: Option<String>,
}

/// Stable wire discriminator for additive write-outcome fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcomeKind {
    Delivered,
    Rejected,
    Unknown,
}

/// Internal result of a platform write. `Unknown` prevents unsafe retries when
/// a timed-out POST may already have reached the platform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Delivered {
        message_id: Option<String>,
    },
    Rejected {
        code: String,
        message: String,
        retry_after_ms: Option<u64>,
    },
    Unknown {
        code: String,
        message: String,
    },
}

/// Response from gateway back to OAB for commands and acknowledged writes.
/// The legacy fields remain required; outcome metadata is additive so old peers
/// can ignore it and new peers can distinguish rejection from uncertainty.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatewayResponse {
    pub schema: String,
    pub request_id: String,
    pub success: bool,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<WriteOutcomeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// Normalized result for `materialize_attachment`; absent for writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Attachment>,
}

impl GatewayResponse {
    pub fn from_write_outcome(request_id: impl Into<String>, outcome: WriteOutcome) -> Self {
        let request_id = request_id.into();
        match outcome {
            WriteOutcome::Delivered { message_id } => Self {
                schema: "openab.gateway.response.v1".into(),
                request_id,
                success: true,
                thread_id: None,
                message_id,
                error: None,
                outcome: Some(WriteOutcomeKind::Delivered),
                error_code: None,
                retry_after_ms: None,
                attachment: None,
            },
            WriteOutcome::Rejected {
                code,
                message,
                retry_after_ms,
            } => Self {
                schema: "openab.gateway.response.v1".into(),
                request_id,
                success: false,
                thread_id: None,
                message_id: None,
                error: Some(message),
                outcome: Some(WriteOutcomeKind::Rejected),
                error_code: Some(code),
                retry_after_ms,
                attachment: None,
            },
            WriteOutcome::Unknown { code, message } => Self {
                schema: "openab.gateway.response.v1".into(),
                request_id,
                success: false,
                thread_id: None,
                message_id: None,
                error: Some(message),
                outcome: Some(WriteOutcomeKind::Unknown),
                error_code: Some(code),
                retry_after_ms: None,
                attachment: None,
            },
        }
    }

    pub fn from_attachment(request_id: impl Into<String>, attachment: Attachment) -> Self {
        Self {
            schema: "openab.gateway.response.v1".into(),
            request_id: request_id.into(),
            success: true,
            thread_id: None,
            message_id: None,
            error: None,
            outcome: None,
            error_code: None,
            retry_after_ms: None,
            attachment: Some(attachment),
        }
    }

    pub fn from_command_error(
        request_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: "openab.gateway.response.v1".into(),
            request_id: request_id.into(),
            success: false,
            thread_id: None,
            message_id: None,
            error: Some(message.into()),
            outcome: None,
            error_code: Some(code.into()),
            retry_after_ms: None,
            attachment: None,
        }
    }

    pub fn write_outcome(&self) -> WriteOutcome {
        match self.outcome {
            Some(WriteOutcomeKind::Delivered) => WriteOutcome::Delivered {
                message_id: self.message_id.clone(),
            },
            Some(WriteOutcomeKind::Rejected) => WriteOutcome::Rejected {
                code: self.error_code.clone().unwrap_or_else(|| "rejected".into()),
                message: self
                    .error
                    .clone()
                    .unwrap_or_else(|| "gateway rejected write".into()),
                retry_after_ms: self.retry_after_ms,
            },
            Some(WriteOutcomeKind::Unknown) => WriteOutcome::Unknown {
                code: self.error_code.clone().unwrap_or_else(|| "unknown".into()),
                message: self
                    .error
                    .clone()
                    .unwrap_or_else(|| "gateway write outcome is unknown".into()),
            },
            None if self.success => WriteOutcome::Delivered {
                message_id: self.message_id.clone(),
            },
            None => WriteOutcome::Rejected {
                code: "legacy_failure".into(),
                message: self
                    .error
                    .clone()
                    .unwrap_or_else(|| "gateway reported failure".into()),
                retry_after_ms: None,
            },
        }
    }
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
            scope: None,
            recipient: None,
            mention_entities: Vec::new(),
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn legacy_response_deserializes_without_outcome_fields() {
        let response: GatewayResponse = serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.response.v1",
            "request_id": "req-1",
            "success": true,
            "thread_id": null,
            "message_id": "activity-1",
            "error": null
        }))
        .unwrap();

        assert_eq!(
            response.write_outcome(),
            WriteOutcome::Delivered {
                message_id: Some("activity-1".into())
            }
        );
        let encoded = serde_json::to_value(response).unwrap();
        assert!(encoded.get("outcome").is_none());
        assert!(encoded.get("error_code").is_none());
        assert!(encoded.get("retry_after_ms").is_none());
    }

    #[test]
    fn structured_write_outcomes_round_trip() {
        let outcomes = [
            WriteOutcome::Delivered {
                message_id: Some("activity-2".into()),
            },
            WriteOutcome::Rejected {
                code: "rate_limited".into(),
                message: "retry later".into(),
                retry_after_ms: Some(750),
            },
            WriteOutcome::Unknown {
                code: "request_timeout".into(),
                message: "delivery may have completed".into(),
            },
        ];

        for (index, expected) in outcomes.into_iter().enumerate() {
            let response =
                GatewayResponse::from_write_outcome(format!("req-{index}"), expected.clone());
            let json = serde_json::to_string(&response).unwrap();
            let decoded: GatewayResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded.write_outcome(), expected);
        }
    }

    #[test]
    fn structured_response_remains_decodable_by_legacy_peer() {
        #[derive(serde::Deserialize)]
        struct LegacyResponse {
            schema: String,
            request_id: String,
            success: bool,
            message_id: Option<String>,
            error: Option<String>,
        }

        let response = GatewayResponse::from_write_outcome(
            "req-legacy",
            WriteOutcome::Unknown {
                code: "request_timeout".into(),
                message: "delivery may have completed".into(),
            },
        );
        let legacy: LegacyResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(legacy.schema, "openab.gateway.response.v1");
        assert_eq!(legacy.request_id, "req-legacy");
        assert!(!legacy.success);
        assert!(legacy.message_id.is_none());
        assert_eq!(legacy.error.as_deref(), Some("delivery may have completed"));
    }

    #[test]
    fn command_target_field_is_additive_and_legacy_decodable() -> anyhow::Result<()> {
        #[derive(serde::Deserialize)]
        struct LegacyReply {
            reply_to: String,
            command: Option<String>,
        }

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "event-1".into(),
            platform: "teams".into(),
            channel: ReplyChannel {
                id: "conversation-1".into(),
                thread_id: None,
            },
            content: Content {
                content_type: "text".into(),
                text: "updated".into(),
                attachments: Vec::new(),
            },
            command: Some("edit_message".into()),
            request_id: Some("request-1".into()),
            quote_message_id: None,
            target_message_id: Some("activity-1".into()),
            attachment_ref: None,
        };
        let json = serde_json::to_string(&reply)?;
        let legacy: LegacyReply = serde_json::from_str(&json)?;
        assert_eq!(legacy.reply_to, "event-1");
        assert_eq!(legacy.command.as_deref(), Some("edit_message"));

        let decoded_without_target: GatewayReply = serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.reply.v1",
            "reply_to": "legacy-activity",
            "platform": "teams",
            "channel": { "id": "conversation-1", "thread_id": null },
            "content": { "type": "text", "text": "updated", "attachments": [] },
            "command": "edit_message",
            "request_id": null,
            "quote_message_id": null
        }))?;
        assert!(decoded_without_target.target_message_id.is_none());
        assert!(decoded_without_target.attachment_ref.is_none());
        assert_eq!(decoded_without_target.reply_to, "legacy-activity");
        Ok(())
    }

    #[test]
    fn attachment_materialization_fields_are_additive_and_bounded_envelopes() -> anyhow::Result<()>
    {
        #[derive(serde::Deserialize)]
        struct LegacyAttachment {
            filename: String,
            mime_type: String,
            #[serde(default)]
            data: String,
        }

        let metadata = Attachment {
            attachment_type: "image".into(),
            filename: "image.png".into(),
            mime_type: "image/png".into(),
            reference: Some("att_opaque".into()),
            data: String::new(),
            size: 0,
            path: None,
            status: None,
        };
        let metadata_json = serde_json::to_string(&metadata)?;
        let legacy: LegacyAttachment = serde_json::from_str(&metadata_json)?;
        assert_eq!(legacy.filename, "image.png");
        assert_eq!(legacy.mime_type, "image/png");
        assert!(legacy.data.is_empty());
        assert!(!metadata_json.contains("http"));

        let materialized = Attachment {
            reference: None,
            data: "aGVsbG8=".into(),
            size: 5,
            ..metadata
        };
        let response = GatewayResponse::from_attachment("request-1", materialized);
        let decoded: GatewayResponse = serde_json::from_str(&serde_json::to_string(&response)?)?;
        let attachment = decoded
            .attachment
            .ok_or_else(|| anyhow::anyhow!("materialized attachment is missing"))?;
        assert_eq!(attachment.decoded_data()?, b"hello");

        let old_wire: Attachment = serde_json::from_value(serde_json::json!({
            "type": "image",
            "filename": "legacy.png",
            "mime_type": "image/png",
            "data": "",
            "size": 0,
            "path": null,
            "status": null
        }))?;
        assert!(old_wire.reference.is_none());
        Ok(())
    }

    #[test]
    fn typed_scope_and_mentions_are_additive_to_gateway_events() -> anyhow::Result<()> {
        #[derive(serde::Deserialize)]
        struct LegacyEvent {
            schema: String,
            event_id: String,
            mentions: Vec<String>,
            message_id: String,
        }

        let mut event = GatewayEvent::new(
            "teams",
            ChannelInfo {
                id: "conversation-1".into(),
                channel_type: "channel".into(),
                thread_id: None,
            },
            SenderInfo {
                id: "user-1".into(),
                name: "Alice".into(),
                display_name: "Alice".into(),
                is_bot: false,
            },
            "<at>OpenAB</at> hello",
            "activity-1",
            vec!["bot-1".into()],
        );
        event.scope = Some(GatewayScope {
            tenant_id: Some("tenant-1".into()),
            team_id: Some("team-1".into()),
            channel_id: Some("channel-1".into()),
            conversation_type: "channel".into(),
            trust_scope_id: "teams:tenant-1:team:team-1:channel:channel-1".into(),
            is_dm: false,
        });
        event.recipient = Some(RecipientInfo {
            id: "bot-1".into(),
            name: "OpenAB".into(),
        });
        event.mention_entities = vec![MentionInfo {
            id: "bot-1".into(),
            text: "<at>OpenAB</at>".into(),
        }];

        let json = serde_json::to_string(&event)?;
        let legacy: LegacyEvent = serde_json::from_str(&json)?;
        assert_eq!(legacy.schema, "openab.gateway.event.v1");
        assert_eq!(legacy.event_id, event.event_id);
        assert_eq!(legacy.mentions, vec!["bot-1"]);
        assert_eq!(legacy.message_id, "activity-1");

        let old_wire = serde_json::json!({
            "schema": "openab.gateway.event.v1",
            "event_id": "event-legacy",
            "timestamp": "2026-08-07T00:00:00Z",
            "platform": "teams",
            "event_type": "message",
            "channel": { "id": "conversation-1", "type": "personal", "thread_id": null },
            "sender": { "id": "user-1", "name": "Alice", "display_name": "Alice", "is_bot": false },
            "content": { "type": "text", "text": "hello" },
            "mentions": [],
            "message_id": "activity-legacy"
        });
        let decoded: GatewayEvent = serde_json::from_value(old_wire)?;
        assert!(decoded.scope.is_none());
        assert!(decoded.recipient.is_none());
        assert!(decoded.mention_entities.is_empty());
        Ok(())
    }

    #[test]
    fn reaction_support_capability_is_additive_for_old_peers() {
        #[derive(serde::Deserialize)]
        struct LegacyCapabilities {
            status_backend: StatusBackend,
        }

        let modern = AdapterCapabilities {
            supports_reactions: true,
            status_backend: StatusBackend::Reactions,
            ..AdapterCapabilities::default()
        };
        let json = serde_json::to_string(&modern).unwrap();
        let legacy: LegacyCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(legacy.status_backend, StatusBackend::Reactions);

        let old_wire = serde_json::json!({ "status_backend": "reactions" });
        let decoded: AdapterCapabilities = serde_json::from_value(old_wire).unwrap();
        assert!(!decoded.supports_reactions);
        assert_eq!(decoded.status_backend, StatusBackend::Reactions);
    }

    #[test]
    fn conversation_registry_capability_is_additive_and_fail_closed() {
        #[derive(serde::Deserialize)]
        struct LegacyCapabilities {
            send_ack: bool,
        }

        let modern = AdapterCapabilities {
            send_ack: true,
            supports_conversation_registry: true,
            ..AdapterCapabilities::default()
        };
        let json = serde_json::to_string(&modern).unwrap();
        let legacy: LegacyCapabilities = serde_json::from_str(&json).unwrap();
        assert!(legacy.send_ack);

        let old_wire = serde_json::json!({ "send_ack": true });
        let decoded: AdapterCapabilities = serde_json::from_value(old_wire).unwrap();
        assert!(!decoded.supports_conversation_registry);
    }

    #[test]
    fn utf16_message_limit_round_trips_without_protocol_change() -> anyhow::Result<()> {
        let value = MessageLimit::Utf16Bytes {
            max: TEAMS_TEXT_UTF16_BUDGET_BYTES,
        };
        let json = serde_json::to_value(value)?;
        assert_eq!(
            json,
            serde_json::json!({
                "unit": "utf16_bytes",
                "max": 80_000,
            })
        );
        assert_eq!(serde_json::from_value::<MessageLimit>(json)?, value);
        Ok(())
    }

    #[test]
    fn missing_capability_fields_default_fail_closed() {
        let capabilities: AdapterCapabilities = serde_json::from_str("{}").unwrap();
        assert!(!capabilities.send_ack);
        assert!(!capabilities.edit_ack);
        assert!(!capabilities.delete_ack);
        assert!(!capabilities.supports_target_message_id);
        assert!(!capabilities.supports_reactions);
        assert!(!capabilities.supports_attachment_materialization);
        assert!(!capabilities.supports_conversation_registry);
        assert!(!capabilities.can_edit);
        assert!(!capabilities.can_delete);
        assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        assert_eq!(capabilities.status_backend, StatusBackend::None);
        assert_eq!(
            capabilities.message_limit,
            MessageLimit::Characters { max: 4096 }
        );
    }
}

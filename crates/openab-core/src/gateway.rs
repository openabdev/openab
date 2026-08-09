use crate::acp::ContentBlock;
use crate::adapter::{
    AdapterCapabilities, AdapterRouter, ChannelRef, ChatAdapter, MaterializedAttachment,
    MessageLimit, MessageRef, SenderContext, StatusBackend, StreamingMode, WriteFailure,
    WriteOutcome, WriteOutcomeKind,
};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

const LEGACY_GATEWAY_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ATTACHMENT_MATERIALIZATION_RESPONSE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(45);
const GATEWAY_WS_MESSAGE_LIMIT: usize = 8 * 1024 * 1024;

fn write_failure(outcome: WriteOutcome) -> anyhow::Error {
    WriteFailure::new(outcome).into()
}

fn unknown_write_failure(code: &str, message: impl Into<String>) -> anyhow::Error {
    write_failure(WriteOutcome::Unknown {
        code: code.to_owned(),
        message: message.into(),
    })
}

fn command_target_fields(
    msg: &MessageRef,
    negotiated: bool,
    capabilities: &AdapterCapabilities,
) -> (String, Option<String>) {
    if negotiated && capabilities.supports_target_message_id {
        (
            msg.channel.origin_event_id.clone().unwrap_or_default(),
            Some(msg.message_id.clone()),
        )
    } else {
        // Old Gateways know only the overloaded command form where `reply_to`
        // carries the platform message target.
        (msg.message_id.clone(), None)
    }
}

/// Capability fallback used only when the peer does not negotiate a hello.
/// It preserves the pre-handshake behavior while keeping platform identity out
/// of the write and streaming control paths themselves.
fn legacy_gateway_capabilities(
    platform: &str,
    streaming: bool,
    streaming_placeholder: bool,
) -> AdapterCapabilities {
    // Preserve the pre-handshake platform behavior exactly. ACP was already
    // forced send-once by the router; LINE and LINE WORKS were the only legacy
    // gateway platforms on the non-editable allowlist.
    let can_edit = !matches!(platform, "line" | "lineworks" | "acp");
    AdapterCapabilities {
        send_ack: false,
        edit_ack: platform == "feishu",
        delete_ack: false,
        supports_target_message_id: false,
        supports_reactions: true,
        supports_attachment_materialization: false,
        can_edit,
        can_delete: platform == "feishu",
        streaming_mode: if streaming && can_edit {
            StreamingMode::Edit
        } else {
            StreamingMode::Disabled
        },
        show_streaming_placeholder: streaming_placeholder,
        message_limit: if platform == "acp" {
            MessageLimit::Unlimited
        } else {
            MessageLimit::Characters { max: 4096 }
        },
        status_backend: StatusBackend::Reactions,
    }
}

fn teams_message_status_supported(
    negotiated: bool,
    capabilities: &AdapterCapabilities,
) -> bool {
    negotiated
        && capabilities.send_ack
        && capabilities.edit_ack
        && capabilities.delete_ack
        && capabilities.supports_target_message_id
        && capabilities.can_edit
        && capabilities.can_delete
}

fn normalize_reaction_support(capabilities: &mut AdapterCapabilities) {
    capabilities.supports_reactions |=
        capabilities.status_backend == StatusBackend::Reactions;
}

fn teams_progressive_response_supported(
    negotiated: bool,
    capabilities: &AdapterCapabilities,
) -> bool {
    negotiated
        && capabilities.send_ack
        && capabilities.edit_ack
        && capabilities.delete_ack
        && capabilities.supports_target_message_id
        && capabilities.can_edit
        && capabilities.can_delete
        && capabilities.show_streaming_placeholder
}

/// Apply the same fail-closed Teams progressive-response predicate in
/// Standalone and Unified deployment modes.
pub fn apply_teams_progressive_capabilities(
    available: bool,
    enabled: bool,
    capabilities: &mut AdapterCapabilities,
) {
    capabilities.streaming_mode =
        if enabled && teams_progressive_response_supported(available, capabilities) {
            StreamingMode::Edit
        } else {
            StreamingMode::Disabled
        };
}

fn apply_teams_processing_indicator(
    negotiated: bool,
    enabled: bool,
    capabilities: &mut AdapterCapabilities,
) {
    if !enabled {
        return;
    }
    capabilities.status_backend =
        if teams_message_status_supported(negotiated, capabilities) {
            StatusBackend::Message
        } else {
            StatusBackend::None
        };
}

/// Shared filter parameters for gateway event gating.
/// Used by both `run_gateway_adapter` (WebSocket) and `process_gateway_event` (unified).
struct EventFilterParams<'a> {
    allow_all_channels: bool,
    allowed_channels: &'a HashSet<String>,
    allow_all_users: bool,
    allowed_users: &'a HashSet<String>,
    allow_bot_messages: bool,
    trusted_bot_ids: &'a HashSet<String>,
    bot_username: Option<&'a str>,
}

/// Returns `true` if the event should be skipped (filtered out).
fn should_skip_event(event: &GatewayEvent, filter: &EventFilterParams) -> bool {
    // Bot filter
    if event.sender.is_bot && !filter.allow_bot_messages && !filter.trusted_bot_ids.contains(&event.sender.id) {
        tracing::info!(sender = %event.sender.id, "gateway: bot not in trusted_bot_ids, skipping");
        return true;
    }
    // Channel allowlist
    if !filter.allow_all_channels && !filter.allowed_channels.contains(&event.channel.id) {
        tracing::info!(channel = %redact_channel(&event.channel.id), "gateway: channel not in allowed_channels, skipping");
        return true;
    }
    // User allowlist
    if !filter.allow_all_users && !filter.allowed_users.contains(&event.sender.id) {
        tracing::info!(sender = %event.sender.id, "gateway: user not in allowed_users, skipping");
        return true;
    }
    // Teams trusts structured mention entity IDs, never display text. Personal
    // chat needs no mention; groupChat/channel always require a recipient
    // mention and do not gain an ambient/thread bypass.
    if event.platform.eq_ignore_ascii_case("teams") {
        if let Some(scope) = event.scope.as_ref() {
            return match scope.conversation_type.as_str() {
                "personal" => !scope.is_dm,
                "groupChat" | "channel" if !scope.is_dm => event
                    .recipient
                    .as_ref()
                    .map(|recipient| recipient.id.as_str())
                    .filter(|id| !id.trim().is_empty())
                    .is_none_or(|recipient_id| {
                        !event
                            .mentions
                            .iter()
                            .any(|mention_id| mention_id == recipient_id)
                    }),
                _ => true,
            };
        }
    }

    // Legacy/non-Teams @mention gating retains the existing group behavior.
    let is_group =
        event.channel.channel_type == "group" || event.channel.channel_type == "supergroup";
    let in_thread = event.channel.thread_id.is_some();
    if is_group && !in_thread {
        if let Some(bot_name) = filter.bot_username {
            if !event.mentions.iter().any(|m| m == bot_name) {
                return true;
            }
        }
    }
    false
}

// --- Gateway event/reply schemas (mirrors gateway service) ---

#[derive(Clone, Debug, Deserialize)]
struct GatewayEvent {
    #[allow(dead_code)]
    schema: String,
    event_id: String,
    #[allow(dead_code)]
    timestamp: String,
    platform: String,
    channel: GwChannel,
    sender: GwSender,
    content: GwContent,
    #[serde(default)]
    mentions: Vec<String>,
    message_id: String,
    #[serde(default)]
    scope: Option<GwScope>,
    #[serde(default)]
    recipient: Option<GwRecipient>,
    #[serde(default)]
    mention_entities: Vec<GwMention>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
struct GwScope {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    conversation_type: String,
    trust_scope_id: String,
    is_dm: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
struct GwRecipient {
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
struct GwMention {
    id: String,
    #[serde(default)]
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GwChannel {
    id: String,
    #[serde(rename = "type")]
    channel_type: String,
    thread_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GwSender {
    id: String,
    name: String,
    display_name: String,
    is_bot: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GwContent {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    content_type: String,
    text: String,
    #[serde(default)]
    attachments: Vec<GwAttachment>,
}

#[derive(Clone, Debug, Deserialize)]
struct GwAttachment {
    #[serde(rename = "type")]
    attachment_type: String,
    filename: String,
    mime_type: String,
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    data: String,
    #[allow(dead_code)]
    size: u64,
    /// Colocate mode: local file path (preferred over base64 `data` when present)
    #[serde(default)]
    path: Option<String>,
    /// Absent = normal. Present = rejected/truncated; human-readable reason.
    #[serde(default)]
    status: Option<String>,
}

/// Teams-specific L2 policy for authenticated typed Gateway scope. Identity
/// remains in the shared trust registry and is evaluated only after this gate.
#[derive(Clone, Debug)]
pub struct TeamsScopePolicy {
    typed_configured: bool,
    allowed_teams: HashSet<String>,
    allowed_channels: HashSet<String>,
    allow_personal: bool,
    allow_group_chats: bool,
    legacy_allow_all_channels: bool,
    legacy_allowed_conversations: HashSet<String>,
}

fn typed_scope_shape_is_valid(conversation_id: &str, channel_type: &str, scope: &GwScope) -> bool {
    let present = |value: Option<&str>| value.is_some_and(|value| !value.trim().is_empty());
    if conversation_id.trim().is_empty()
        || !present(scope.tenant_id.as_deref())
        || scope.trust_scope_id.trim().is_empty()
        || scope.conversation_type != channel_type
    {
        return false;
    }

    match scope.conversation_type.as_str() {
        "personal" => scope.is_dm,
        "groupChat" => !scope.is_dm,
        "channel" => {
            !scope.is_dm
                && present(scope.team_id.as_deref())
                && present(scope.channel_id.as_deref())
        }
        _ => false,
    }
}

impl TeamsScopePolicy {
    pub fn new(
        typed_configured: bool,
        allowed_teams: impl IntoIterator<Item = String>,
        allowed_channels: impl IntoIterator<Item = String>,
        allow_personal: bool,
        allow_group_chats: bool,
        legacy_allow_all_channels: bool,
        legacy_allowed_conversations: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            typed_configured,
            allowed_teams: allowed_teams.into_iter().collect(),
            allowed_channels: allowed_channels.into_iter().collect(),
            allow_personal,
            allow_group_chats,
            legacy_allow_all_channels,
            legacy_allowed_conversations: legacy_allowed_conversations.into_iter().collect(),
        }
    }

    pub fn uses_legacy_fallback(&self) -> bool {
        !self.typed_configured
    }

    pub fn legacy_scope_restricted(&self) -> bool {
        !self.legacy_allow_all_channels
    }

    fn surface_allowed(&self, conversation_id: &str, channel_type: &str, scope: &GwScope) -> bool {
        if !typed_scope_shape_is_valid(conversation_id, channel_type, scope) {
            return false;
        }

        if !self.typed_configured {
            return self.legacy_allow_all_channels
                || self.legacy_allowed_conversations.contains(conversation_id);
        }

        match scope.conversation_type.as_str() {
            "personal" => self.allow_personal,
            "groupChat" => self.allow_group_chats,
            "channel" => {
                (self.allowed_teams.is_empty() && self.allowed_channels.is_empty())
                    || scope
                        .team_id
                        .as_ref()
                        .is_some_and(|team| self.allowed_teams.contains(team))
                    || scope
                        .channel_id
                        .as_ref()
                        .is_some_and(|channel| self.allowed_channels.contains(channel))
            }
            _ => false,
        }
    }
}

impl Default for TeamsScopePolicy {
    fn default() -> Self {
        Self::new(
            false,
            Vec::<String>::new(),
            Vec::<String>::new(),
            true,
            true,
            true,
            Vec::<String>::new(),
        )
    }
}

fn strip_recipient_mention(event: &GatewayEvent) -> String {
    if !event.platform.eq_ignore_ascii_case("teams") {
        return event.content.text.clone();
    }
    let Some(recipient_id) = event
        .recipient
        .as_ref()
        .map(|recipient| recipient.id.as_str())
        .filter(|id| !id.trim().is_empty())
    else {
        return event.content.text.clone();
    };

    let mut ranges = Vec::new();
    let mut cursor = 0;
    for mention in &event.mention_entities {
        if mention.text.is_empty() || cursor > event.content.text.len() {
            continue;
        }
        let Some(relative_start) = event.content.text[cursor..].find(&mention.text) else {
            continue;
        };
        let start = cursor + relative_start;
        let end = start + mention.text.len();
        cursor = end;
        if mention.id == recipient_id {
            ranges.push(start..end);
        }
    }

    let mut prompt = event.content.text.clone();
    for range in ranges.into_iter().rev() {
        prompt.replace_range(range, "");
    }
    prompt.trim().to_owned()
}

#[derive(Serialize)]
struct GatewayReply {
    schema: String,
    reply_to: String,
    platform: String,
    channel: ReplyChannel,
    content: ReplyContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    /// When set, the gateway should send this message as a reply/quote to the specified message ID.
    /// Unlike `reply_to` (routing/dedup identifier for the triggering event), this field controls
    /// the visual reply/quote UI on the platform. Falls back to plain send on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    quote_message_id: Option<String>,
    /// Platform message targeted by an edit/delete/reaction command. New peers
    /// keep `reply_to` as origin event correlation; legacy peers receive the
    /// command target in `reply_to` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    target_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_ref: Option<String>,
}

#[derive(Serialize)]
struct ReplyChannel {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
}

#[derive(Serialize)]
struct ReplyContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GatewayResponse {
    #[allow(dead_code)]
    schema: String,
    request_id: String,
    success: bool,
    thread_id: Option<String>,
    message_id: Option<String>,
    error: Option<String>,
    #[serde(default)]
    outcome: Option<WriteOutcomeKind>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    attachment: Option<GwAttachment>,
}

impl GatewayResponse {
    fn write_outcome(&self) -> WriteOutcome {
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

const CLIENT_HELLO_SCHEMA: &str = "openab.gateway.client_hello.v1";
const GATEWAY_HELLO_SCHEMA: &str = "openab.gateway.hello.v1";
const GATEWAY_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
struct GatewayEnvelope {
    schema: String,
}

#[derive(Debug, Serialize)]
struct GatewayClientHello {
    schema: String,
    protocol_version: u32,
    client_name: Option<String>,
    requested_platforms: Vec<String>,
}

fn build_client_hello() -> GatewayClientHello {
    GatewayClientHello {
        schema: CLIENT_HELLO_SCHEMA.into(),
        protocol_version: GATEWAY_PROTOCOL_VERSION,
        client_name: Some(format!("openab-core/{}", env!("CARGO_PKG_VERSION"))),
        // A standalone Gateway can publish several platforms over one socket,
        // so Core requests the full configured capability map.
        requested_platforms: Vec::new(),
    }
}

#[derive(Clone, Debug, Deserialize)]
struct GatewayHello {
    schema: String,
    protocol_version: u32,
    #[serde(default)]
    capabilities: HashMap<String, AdapterCapabilities>,
    topology: GatewayTopology,
}

#[derive(Clone, Debug, Deserialize)]
struct GatewayTopology {
    active_consumers: usize,
    supported: bool,
    delivery_mode: String,
}

#[derive(Default)]
struct GatewayCapabilityState {
    hello: std::sync::RwLock<Option<GatewayHello>>,
}

impl GatewayCapabilityState {
    fn update(&self, hello: GatewayHello) {
        *self
            .hello
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hello);
    }

    fn topology_supported(&self) -> bool {
        self.hello
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|hello| hello.topology.supported && hello.topology.active_consumers == 1)
    }

    fn resolve(&self, platform: &str, legacy: &AdapterCapabilities) -> (bool, AdapterCapabilities) {
        let hello = self
            .hello
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match hello.as_ref() {
            Some(hello) => (
                true,
                hello
                    .capabilities
                    .get(platform)
                    .cloned()
                    .unwrap_or_default(),
            ),
            None => (false, legacy.clone()),
        }
    }
}

// --- GatewayAdapter: ChatAdapter over WebSocket ---

type PendingRequests = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<GatewayResponse>>>>;

/// Removes a pending attachment request when its future is cancelled by the
/// whole-batch deadline. Normal responses remove the same key in the reader,
/// so this cleanup is a no-op on the success path.
struct PendingAttachmentRequest {
    pending: PendingRequests,
    request_id: String,
}

impl Drop for PendingAttachmentRequest {
    fn drop(&mut self) {
        let pending = self.pending.clone();
        let request_id = self.request_id.clone();
        tokio::spawn(async move {
            pending.lock().await.remove(&request_id);
        });
    }
}

type SharedWsTx = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
>;

struct GatewayAdapterOptions {
    platform_name: &'static str,
    streaming: bool,
    streaming_placeholder: bool,
    telegram_rich_messages: bool,
    teams_processing_indicator: bool,
    teams_streaming: bool,
    teams_inbound_attachments: bool,
    gateway_ack_timeout_secs: u64,
}

pub struct GatewayAdapter {
    ws_tx: SharedWsTx,
    pending: PendingRequests,
    connection_active: Arc<AtomicBool>,
    capability_state: Arc<GatewayCapabilityState>,
    legacy_capabilities: AdapterCapabilities,
    platform_name: &'static str,
    streaming: bool,
    streaming_placeholder: bool,
    telegram_rich_messages: bool,
    teams_processing_indicator: bool,
    teams_streaming: bool,
    teams_inbound_attachments: bool,
    ack_timeout: std::time::Duration,
}

impl GatewayAdapter {
    fn new(
        ws_tx: SharedWsTx,
        pending: PendingRequests,
        connection_active: Arc<AtomicBool>,
        capability_state: Arc<GatewayCapabilityState>,
        options: GatewayAdapterOptions,
    ) -> Self {
        let GatewayAdapterOptions {
            platform_name,
            streaming,
            streaming_placeholder,
            telegram_rich_messages,
            teams_processing_indicator,
            teams_streaming,
            teams_inbound_attachments,
            gateway_ack_timeout_secs,
        } = options;
        Self {
            ws_tx,
            pending,
            connection_active,
            capability_state,
            legacy_capabilities: legacy_gateway_capabilities(
                platform_name,
                streaming,
                streaming_placeholder,
            ),
            platform_name,
            streaming,
            streaming_placeholder,
            telegram_rich_messages,
            teams_processing_indicator,
            teams_streaming,
            teams_inbound_attachments,
            ack_timeout: std::time::Duration::from_secs(gateway_ack_timeout_secs.max(1)),
        }
    }

    fn resolved_capabilities_with_mode(&self, platform: &str) -> (bool, AdapterCapabilities) {
        let (negotiated, mut capabilities) = self
            .capability_state
            .resolve(platform, &self.legacy_capabilities);
        let teams = platform.eq_ignore_ascii_case("teams");
        if teams {
            capabilities.supports_attachment_materialization &= self.teams_inbound_attachments
                && negotiated
                && self.capability_state.topology_supported();
            // Teams has an independent default-off opt-in. Do not inherit the
            // generic Gateway streaming or placeholder switches.
            apply_teams_progressive_capabilities(
                negotiated,
                self.teams_streaming,
                &mut capabilities,
            );
        } else {
            if !self.streaming {
                capabilities.streaming_mode = StreamingMode::Disabled;
            }
            capabilities.show_streaming_placeholder &= self.streaming_placeholder;
        }
        // Older peers represented reaction availability only through the
        // selected status backend. Normalize that shape before a configured
        // processing message overrides transient progress selection.
        normalize_reaction_support(&mut capabilities);
        if teams {
            apply_teams_processing_indicator(
                negotiated,
                self.teams_processing_indicator,
                &mut capabilities,
            );
        }
        (negotiated, capabilities)
    }

    fn resolved_capabilities(&self, platform: &str) -> AdapterCapabilities {
        self.resolved_capabilities_with_mode(platform).1
    }

    /// Internal helper for send_message / send_message_with_reply.
    async fn send_gateway_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        quote_message_id: Option<&str>,
    ) -> Result<MessageRef> {
        let (negotiated, capabilities) = self.resolved_capabilities_with_mode(&channel.platform);
        let required_ack = negotiated && capabilities.send_ack;
        // Preserve legacy streaming correlation without turning a missing ACK
        // into failure. New peers request an ACK only when it was advertised.
        let request_ack = required_ack || (!negotiated && self.streaming);
        let req_id = request_ack.then(|| format!("req_{}", uuid::Uuid::new_v4()));
        let pending_rx = if let Some(ref id) = req_id {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.lock().await.insert(id.clone(), tx);
            Some(rx)
        } else {
            None
        };
        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to: channel.origin_event_id.clone().unwrap_or_default(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: content.into(),
            },
            command: None,
            request_id: req_id.clone(),
            quote_message_id: quote_message_id.map(|s| s.to_string()),
            target_message_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(e) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            if let Some(ref id) = req_id {
                self.pending.lock().await.remove(id);
            }
            return Err(unknown_write_failure("gateway_send_failed", e.to_string()));
        }
        let msg_id = if let (Some(rx), Some(ref id)) = (pending_rx, &req_id) {
            let response_timeout = if required_ack {
                self.ack_timeout
            } else {
                LEGACY_GATEWAY_REPLY_TIMEOUT
            };
            match tokio::time::timeout(response_timeout, rx).await {
                Ok(Ok(resp)) => match resp.write_outcome() {
                    WriteOutcome::Delivered { message_id } => match message_id {
                        Some(message_id) if !message_id.is_empty() => message_id,
                        _ if required_ack => {
                            return Err(unknown_write_failure(
                                "missing_message_id",
                                "gateway delivered send without a message id",
                            ));
                        }
                        _ => "gw_sent".into(),
                    },
                    WriteOutcome::Rejected {
                        code,
                        message,
                        retry_after_ms,
                    } => {
                        warn!(
                            request_id = %id,
                            error_code = %code,
                            retry_after_ms,
                            error = %message,
                            "gateway rejected write"
                        );
                        return Err(write_failure(WriteOutcome::Rejected {
                            code,
                            message,
                            retry_after_ms,
                        }));
                    }
                    WriteOutcome::Unknown { code, message } => {
                        warn!(
                            request_id = %id,
                            error_code = %code,
                            error = %message,
                            "gateway write outcome unknown; not retrying"
                        );
                        return Err(write_failure(WriteOutcome::Unknown { code, message }));
                    }
                },
                Ok(Err(_)) if required_ack => {
                    return Err(unknown_write_failure(
                        "send_ack_channel_closed",
                        "required gateway ACK channel closed",
                    ));
                }
                Ok(Err(_)) => {
                    warn!(request_id = %id, "legacy gateway response channel closed");
                    "gw_sent".into()
                }
                Err(_) if required_ack => {
                    self.pending.lock().await.remove(id);
                    return Err(unknown_write_failure(
                        "send_ack_timeout",
                        "required gateway ACK timed out",
                    ));
                }
                Err(_) => {
                    warn!(request_id = %id, "legacy gateway reply timed out");
                    self.pending.lock().await.remove(id);
                    "gw_sent".into()
                }
            }
        } else {
            "gw_sent".into()
        };
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: msg_id,
        })
    }

    async fn request_attachment_materialization(
        &self,
        channel: &ChannelRef,
        reference: &str,
    ) -> Result<MaterializedAttachment> {
        let request_id = format!("req_{}", uuid::Uuid::new_v4());
        let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if !self.connection_active.load(AtomicOrdering::Acquire) {
                anyhow::bail!("attachment materialization connection is unavailable");
            }
            pending.insert(request_id.clone(), pending_tx);
        }
        let _pending_cleanup = PendingAttachmentRequest {
            pending: self.pending.clone(),
            request_id: request_id.clone(),
        };
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: channel.origin_event_id.clone().unwrap_or_default(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: String::new(),
            },
            command: Some("materialize_attachment".into()),
            request_id: Some(request_id.clone()),
            quote_message_id: None,
            target_message_id: None,
            attachment_ref: Some(reference.to_owned()),
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(error) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            self.pending.lock().await.remove(&request_id);
            anyhow::bail!("attachment materialization request failed: {error}");
        }
        let response = match tokio::time::timeout(
            ATTACHMENT_MATERIALIZATION_RESPONSE_TIMEOUT,
            pending_rx,
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => anyhow::bail!("attachment materialization response channel closed"),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                anyhow::bail!("attachment materialization response timed out");
            }
        };
        if !response.success {
            anyhow::bail!(
                "attachment materialization rejected: {}",
                response.error_code.as_deref().unwrap_or("gateway_rejected")
            );
        }
        let attachment = response
            .attachment
            .ok_or_else(|| anyhow::anyhow!("materialization response has no attachment"))?;
        if attachment.reference.is_some() || attachment.path.is_some() {
            anyhow::bail!("materialization response contains an invalid attachment envelope");
        }
        if !matches!(attachment.attachment_type.as_str(), "image" | "text_file")
            || attachment.filename.chars().count() > 200
            || attachment.filename.chars().any(char::is_control)
            || attachment.mime_type.len() > 128
            || attachment.mime_type.chars().any(char::is_control)
            || attachment
                .status
                .as_ref()
                .is_some_and(|status| status.len() > 256 || status.chars().any(char::is_control))
        {
            anyhow::bail!("materialization response contains invalid attachment metadata");
        }
        let data = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(&attachment.data)
                .map_err(|_| anyhow::anyhow!("materialization response has malformed data"))?
        };
        if attachment.status.is_some() {
            if !data.is_empty() {
                anyhow::bail!("rejected materialization response contains payload data");
            }
        } else if attachment.size != data.len() as u64 {
            anyhow::bail!("materialization response size does not match its payload");
        }
        Ok(MaterializedAttachment {
            attachment_type: attachment.attachment_type,
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            data,
            size: attachment.size,
            status: attachment.status,
        })
    }
}

/// Send a fire-and-forget reply via the shared WebSocket (no request-response).
/// Used for slash command responses where we don't need message_id back.
async fn send_fire_and_forget(
    ws_tx: &SharedWsTx,
    channel: &ChannelRef,
    content: &str,
) -> Result<()> {
    let reply = GatewayReply {
        attachment_ref: None,
        schema: "openab.gateway.reply.v1".into(),
        reply_to: channel.origin_event_id.clone().unwrap_or_default(),
        platform: channel.platform.clone(),
        channel: ReplyChannel {
            id: channel.channel_id.clone(),
            thread_id: channel.thread_id.clone(),
        },
        content: ReplyContent {
            content_type: "text".into(),
            text: content.into(),
        },
        command: None,
        request_id: None,
        quote_message_id: None,
        target_message_id: None,
    };
    let json = serde_json::to_string(&reply)?;
    ws_tx.lock().await.send(Message::Text(json)).await?;
    Ok(())
}

/// Handle `/models` or `/agents` text commands for gateway platforms.
/// Returns the response message, or None if the command was not recognized.
///
/// Supported syntax:
///   /model list       — numbered list of available models
///   /model set <name> — switch by exact name or number
///   /models           — alias of /model list
///   /agent list       — numbered list of available agents
///   /agent set <name> — switch by exact name or number
///   /agents           — alias of /agent list
async fn handle_config_command(
    trimmed: &str,
    router: &AdapterRouter,
    thread_key: &str,
) -> Option<String> {
    // Parse command: /model <action> <arg> or /models (alias)
    let (category, label, action, arg) = if trimmed == "/models" {
        ("model", "model", "list", "")
    } else if trimmed == "/agents" {
        ("agent", "agent", "list", "")
    } else if trimmed.starts_with("/model ") {
        let rest = trimmed.strip_prefix("/model ").unwrap().trim();
        let (action, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        ("model", "model", action, arg.trim())
    } else if trimmed.starts_with("/agent ") {
        let rest = trimmed.strip_prefix("/agent ").unwrap().trim();
        let (action, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        ("agent", "agent", action, arg.trim())
    } else if trimmed == "/model" {
        ("model", "model", "list", "")
    } else if trimmed == "/agent" {
        ("agent", "agent", "list", "")
    } else {
        return None;
    };

    // Support both "agent" and "mode" categories (kiro-cli vs cursor-agent)
    let categories: &[&str] = if category == "agent" {
        &["agent", "mode"]
    } else {
        &[category]
    };

    let options = router.pool().get_config_options(thread_key).await;
    let filtered: Vec<_> = options
        .iter()
        .filter(|o| {
            o.category
                .as_deref()
                .is_some_and(|c| categories.contains(&c))
        })
        .collect();

    if filtered.is_empty() {
        return Some(format!(
            "⚠️ No {label} options available. Start a conversation first."
        ));
    }

    // Collect all values with index for numbered list / set-by-number
    let mut all_values: Vec<(String, String, String, bool)> = Vec::new(); // (config_id, value, name, is_current)
    for opt in &filtered {
        for v in &opt.options {
            all_values.push((
                opt.id.clone(),
                v.value.clone(),
                v.name.clone(),
                v.value == opt.current_value,
            ));
        }
    }

    match action {
        "list" => {
            let mut lines = vec![format!("🔧 Available {label}s:")];
            for (i, (_, _, name, is_current)) in all_values.iter().enumerate() {
                let marker = if *is_current { " ✅" } else { "" };
                lines.push(format!("  {}. {}{}", i + 1, name, marker));
            }
            lines.push(format!("\nUsage: /{label} set <number or name>"));
            Some(lines.join("\n"))
        }
        "set" => {
            if arg.is_empty() {
                return Some(format!("Usage: /{label} set <number or name>"));
            }
            // Try number first
            if let Ok(num) = arg.parse::<usize>() {
                if num >= 1 && num <= all_values.len() {
                    let (ref config_id, ref value, ref name, _) = all_values[num - 1];
                    return match router
                        .pool()
                        .set_config_option(thread_key, config_id, value)
                        .await
                    {
                        Ok(_) => Some(format!("✅ Switched to **{name}**")),
                        Err(e) => Some(format!("❌ Failed to switch: {e}")),
                    };
                } else {
                    return Some(format!("⚠️ Invalid number. Use 1–{}.", all_values.len()));
                }
            }
            // Exact match on value or name
            let arg_lower = arg.to_lowercase();
            for (config_id, value, name, _) in &all_values {
                if value.to_lowercase() == arg_lower || name.to_lowercase() == arg_lower {
                    return match router
                        .pool()
                        .set_config_option(thread_key, config_id, value)
                        .await
                    {
                        Ok(_) => Some(format!("✅ Switched to **{name}**")),
                        Err(e) => Some(format!("❌ Failed to switch: {e}")),
                    };
                }
            }
            Some(format!(
                "⚠️ No {label} matching \"{arg}\". Use /{label} list to see options."
            ))
        }
        _ => Some(format!(
            "Unknown action \"{action}\". Usage: /{label} list | /{label} set <name>"
        )),
    }
}

#[async_trait]
impl ChatAdapter for GatewayAdapter {
    fn platform(&self) -> &'static str {
        self.platform_name
    }

    fn message_limit(&self) -> usize {
        4096 // Legacy conservative limit; negotiated capabilities are platform-aware.
    }

    fn capabilities(&self, platform: &str) -> AdapterCapabilities {
        self.resolved_capabilities(platform)
    }

    async fn materialize_attachment(
        &self,
        channel: &ChannelRef,
        reference: &str,
    ) -> Result<MaterializedAttachment> {
        let (negotiated, capabilities) = self.resolved_capabilities_with_mode(&channel.platform);
        if !negotiated
            || !self.capability_state.topology_supported()
            || !capabilities.supports_attachment_materialization
        {
            anyhow::bail!("attachment materialization capability is unavailable");
        }
        if channel
            .origin_event_id
            .as_deref()
            .is_none_or(|event_id| event_id.trim().is_empty())
            || reference.trim().is_empty()
            || reference.len() > 128
            || !reference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            anyhow::bail!("attachment materialization route is unavailable");
        }
        self.request_attachment_materialization(channel, reference)
            .await
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        self.send_gateway_reply(channel, content, None).await
    }

    async fn send_message_with_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        reply_to_message_id: &str,
    ) -> Result<MessageRef> {
        self.send_gateway_reply(channel, content, Some(reply_to_message_id)).await
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        // Send create_topic command to gateway
        let req_id = format!("req_{}", uuid::Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(req_id.clone(), tx);

        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to: String::new(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: None,
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: title.into(),
            },
            command: Some("create_topic".into()),
            request_id: Some(req_id.clone()),
            quote_message_id: None,
            target_message_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;

        // Wait for response (5s timeout)
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(resp)) if resp.success => Ok(ChannelRef {
                platform: channel.platform.clone(),
                channel_id: channel.channel_id.clone(),
                thread_id: resp.thread_id,
                parent_id: None,
                origin_event_id: channel.origin_event_id.clone(),
            }),
            Ok(Ok(resp)) => {
                warn!(err = ?resp.error, "create_topic failed, falling back to same channel");
                Ok(channel.clone())
            }
            _ => {
                warn!("create_topic timeout, falling back to same channel");
                self.pending.lock().await.remove(&req_id);
                Ok(channel.clone())
            }
        }
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let (negotiated, capabilities) =
            self.resolved_capabilities_with_mode(&msg.channel.platform);
        let (reply_to, target_message_id) = command_target_fields(msg, negotiated, &capabilities);
        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to,
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: emoji.into(),
            },
            command: Some("add_reaction".into()),
            quote_message_id: None,
            target_message_id,
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let (negotiated, capabilities) =
            self.resolved_capabilities_with_mode(&msg.channel.platform);
        let (reply_to, target_message_id) = command_target_fields(msg, negotiated, &capabilities);
        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to,
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: emoji.into(),
            },
            command: Some("remove_reaction".into()),
            quote_message_id: None,
            target_message_id,
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        // Use a short request/response cycle so we can react to platform-level
        // edit failures (e.g. Feishu's 20-edits-per-message cap, errcode 230072).
        // Without this, edit_message was fire-and-forget and core never saw cap
        // signals — cosmetic streaming would keep flushing forever and the final
        // edit fallback to send_message could not trigger.
        //
        // Only negotiated/legacy capabilities that explicitly advertise edit
        // acknowledgements pay the response-wait cost.
        const LEGACY_EDIT_RESPONSE_TIMEOUT_MS: u64 = 800;
        let (negotiated, capabilities) =
            self.resolved_capabilities_with_mode(&msg.channel.platform);
        let required_ack = negotiated && capabilities.edit_ack;
        let needs_response =
            required_ack || (!negotiated && self.streaming && capabilities.edit_ack);
        let response_timeout = if required_ack {
            self.ack_timeout
        } else {
            std::time::Duration::from_millis(LEGACY_EDIT_RESPONSE_TIMEOUT_MS)
        };

        let req_id = if needs_response {
            Some(format!("req_{}", uuid::Uuid::new_v4()))
        } else {
            None
        };
        let pending_rx = if let Some(ref id) = req_id {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.lock().await.insert(id.clone(), tx);
            Some(rx)
        } else {
            None
        };
        let (reply_to, target_message_id) = command_target_fields(msg, negotiated, &capabilities);
        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to,
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: content.into(),
            },
            command: Some("edit_message".into()),
            quote_message_id: None,
            target_message_id,
            request_id: req_id.clone(),
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(e) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            if let Some(ref id) = req_id {
                self.pending.lock().await.remove(id);
            }
            return Err(unknown_write_failure(
                "gateway_edit_send_failed",
                e.to_string(),
            ));
        }
        if let (Some(rx), Some(ref id)) = (pending_rx, &req_id) {
            match tokio::time::timeout(response_timeout, rx).await {
                Ok(Ok(resp)) => match resp.write_outcome() {
                    WriteOutcome::Delivered { .. } => Ok(()),
                    WriteOutcome::Rejected {
                        code,
                        message,
                        retry_after_ms,
                    } => {
                        warn!(request_id = %id, error_code = %code, error = %message, "gateway rejected edit");
                        Err(write_failure(WriteOutcome::Rejected {
                            code,
                            message,
                            retry_after_ms,
                        }))
                    }
                    WriteOutcome::Unknown { code, message } => {
                        warn!(request_id = %id, error_code = %code, error = %message, "gateway edit outcome unknown");
                        Err(write_failure(WriteOutcome::Unknown { code, message }))
                    }
                },
                Ok(Err(_)) if required_ack => Err(unknown_write_failure(
                    "edit_ack_channel_closed",
                    "required edit ACK channel closed",
                )),
                Ok(Err(_)) => {
                    tracing::debug!(request_id = %id, "legacy edit response channel closed");
                    Ok(())
                }
                Err(_) if required_ack => {
                    self.pending.lock().await.remove(id);
                    Err(unknown_write_failure(
                        "edit_ack_timeout",
                        "required edit ACK timed out",
                    ))
                }
                Err(_) => {
                    // Legacy Feishu used a short best-effort observation window;
                    // preserve that behavior when no capability was negotiated.
                    self.pending.lock().await.remove(id);
                    Ok(())
                }
            }
        } else {
            // An unadvertised edit remains fire-and-forget with no added latency.
            Ok(())
        }
    }

    /// Override default delete_message (which falls back to edit-to-zero-width)
    /// so platforms with native delete APIs (e.g. Feishu DELETE /im/v1/messages/{id})
    /// can perform real deletions. Critical for the streaming-edit-cap recovery
    /// path: when Feishu's 20-edits-per-message cap is hit and we send full
    /// content as a fresh message, we need to remove the half-edited placeholder
    /// to avoid duplicated content. The default zero-width-edit fallback would
    /// itself fail on a cap-reached message, leaving the placeholder visible.
    ///
    /// Legacy peers remain fire-and-forget. A negotiated peer is awaited only
    /// when it explicitly advertises `delete_ack`.
    async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
        let (negotiated, capabilities) =
            self.resolved_capabilities_with_mode(&msg.channel.platform);
        let required_ack = negotiated && capabilities.delete_ack;
        let request_id = required_ack.then(|| format!("req_{}", uuid::Uuid::new_v4()));
        let pending_rx = if let Some(ref id) = request_id {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.lock().await.insert(id.clone(), tx);
            Some(rx)
        } else {
            None
        };
        let (reply_to, target_message_id) = command_target_fields(msg, negotiated, &capabilities);
        let reply = GatewayReply {
            attachment_ref: None,
            schema: "openab.gateway.reply.v1".into(),
            reply_to,
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: String::new(),
            },
            command: Some("delete_message".into()),
            quote_message_id: None,
            target_message_id,
            request_id: request_id.clone(),
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(error) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            if let Some(ref id) = request_id {
                self.pending.lock().await.remove(id);
            }
            return Err(unknown_write_failure(
                "gateway_delete_send_failed",
                error.to_string(),
            ));
        }

        let (Some(rx), Some(id)) = (pending_rx, request_id) else {
            return Ok(());
        };
        match tokio::time::timeout(self.ack_timeout, rx).await {
            Ok(Ok(response)) => match response.write_outcome() {
                WriteOutcome::Delivered { .. } => Ok(()),
                WriteOutcome::Rejected {
                    code,
                    message,
                    retry_after_ms,
                } => {
                    warn!(request_id = %id, error_code = %code, error = %message, "gateway rejected delete");
                    Err(write_failure(WriteOutcome::Rejected {
                        code,
                        message,
                        retry_after_ms,
                    }))
                }
                WriteOutcome::Unknown { code, message } => {
                    warn!(request_id = %id, error_code = %code, error = %message, "gateway delete outcome unknown");
                    Err(write_failure(WriteOutcome::Unknown { code, message }))
                }
            },
            Ok(Err(_)) => Err(unknown_write_failure(
                "delete_ack_channel_closed",
                "required delete ACK channel closed",
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(unknown_write_failure(
                    "delete_ack_timeout",
                    "required delete ACK timed out",
                ))
            }
        }
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        self.resolved_capabilities(self.platform_name)
            .streaming_mode
            != StreamingMode::Disabled
    }

    fn show_streaming_placeholder(&self) -> bool {
        self.resolved_capabilities(self.platform_name)
            .show_streaming_placeholder
    }

    fn renders_native_tables(&self, _platform: &str) -> bool {
        // Telegram renders markdown tables natively via Rich Messages;
        // skip the table→code-block pre-pass for that platform only when
        // Rich Messages is confirmed enabled.
        self.platform_name == "telegram" && self.telegram_rich_messages
    }
}

// --- Run the gateway adapter (connects to gateway WS, routes events to AdapterRouter) ---

/// Resolved gateway configuration passed to the adapter at startup.
/// Channel/user allowlists are NOT carried here anymore: L2/L3 enforcement for
/// the WebSocket path moved to the shared per-platform trust registry
/// (`AdapterRouter::gate_incoming`), seeded in main.rs with precedence
/// `GATEWAY_*` env < `[gateway]` section < `[<platform>]` section (#1356).
pub struct GatewayParams {
    pub url: String,
    pub platform: String,
    pub token: Option<String>,
    pub bot_username: Option<String>,
    pub allow_bot_messages: bool,
    pub trusted_bot_ids: Vec<String>,
    pub streaming: bool,
    pub streaming_placeholder: bool,
    pub telegram_rich_messages: bool,
    pub teams_processing_indicator: bool,
    pub teams_streaming: bool,
    pub teams_inbound_attachments: bool,
    pub gateway_ack_timeout_secs: u64,
    pub stt: crate::config::SttConfig,
    pub teams_scope_policy: TeamsScopePolicy,
}

pub async fn run_gateway_adapter(
    params: GatewayParams,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    dispatcher: Arc<crate::dispatch::Dispatcher>,
    router: Arc<crate::adapter::AdapterRouter>,
    #[cfg(feature = "filestore")] filestore: Option<Arc<crate::filestore::Filestore>>,
) -> Result<()> {
    let platform: &'static str = Box::leak(params.platform.into_boxed_str());

    // Append auth token as query param if configured
    let gateway_url = params.url;
    let bot_username = params.bot_username;
    let allow_bot_messages = params.allow_bot_messages;
    let trusted_bot_ids: HashSet<String> = params.trusted_bot_ids.into_iter().collect();
    // The platform-aware capability contract decides whether configured
    // streaming is usable. Legacy peers resolve through the conservative
    // fallback; negotiated peers supply this over the hello exchange.
    let streaming = params.streaming;
    let streaming_placeholder = params.streaming_placeholder;
    let telegram_rich_messages = params.telegram_rich_messages;
    let teams_processing_indicator = params.teams_processing_indicator;
    let teams_streaming = params.teams_streaming;
    let teams_inbound_attachments = params.teams_inbound_attachments;
    let gateway_ack_timeout_secs = params.gateway_ack_timeout_secs;
    let stt_config = params.stt;
    let teams_scope_policy = params.teams_scope_policy;

    let connect_url = match &params.token {
        Some(token) => {
            let sep = if gateway_url.contains('?') { "&" } else { "?" };
            format!("{gateway_url}{sep}token={token}")
        }
        None => {
            warn!("gateway.token not set — WebSocket connection is NOT authenticated");
            gateway_url.clone()
        }
    };
    let mut backoff_secs = 1u64;
    const MAX_BACKOFF: u64 = 30;

    loop {
        // Check shutdown before connecting
        if *shutdown_rx.borrow() {
            info!("gateway adapter shutting down");
            return Ok(());
        }

        info!(url = %gateway_url, "connecting to custom gateway");

        let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(GATEWAY_WS_MESSAGE_LIMIT),
            max_frame_size: Some(GATEWAY_WS_MESSAGE_LIMIT),
            ..Default::default()
        };
        let ws_stream = match tokio_tungstenite::connect_async_with_config(
            &connect_url,
            Some(ws_config),
            false,
        )
        .await
        {
            Ok((stream, _)) => {
                backoff_secs = 1; // reset on success
                info!("connected to gateway");
                stream
            }
            Err(e) => {
                error!(err = %e, backoff = backoff_secs, "gateway connection failed, retrying");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = shutdown_rx.changed() => { return Ok(()); }
                }
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let (ws_tx, mut ws_rx) = ws_stream.split();
        let ws_tx: SharedWsTx = Arc::new(Mutex::new(ws_tx));
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let connection_active = Arc::new(AtomicBool::new(true));
        let capability_state = Arc::new(GatewayCapabilityState::default());
        let client_hello = build_client_hello();
        let hello_json = serde_json::to_string(&client_hello)?;
        if let Err(error) = ws_tx.lock().await.send(Message::Text(hello_json)).await {
            warn!(error = %error, "failed to send optional gateway hello; continuing in legacy mode");
        }
        let adapter: Arc<dyn ChatAdapter> = Arc::new(GatewayAdapter::new(
            ws_tx.clone(),
            pending.clone(),
            connection_active.clone(),
            capability_state.clone(),
            GatewayAdapterOptions {
                platform_name: platform,
                streaming,
                streaming_placeholder,
                telegram_rich_messages,
                teams_processing_indicator,
                teams_streaming,
                teams_inbound_attachments,
                gateway_ack_timeout_secs,
            },
        ));
        let slash_ws_tx = ws_tx.clone(); // for fire-and-forget slash command responses
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        // Hoist filter params outside loop — all fields are loop-invariant.
        // Structural gating (bot filter + @mention) stays in should_skip_event.
        // L2 (channel) + L3 (identity) are enforced by the shared ingress gate
        // (`gate_gateway_event`) below — same registry as the unified path —
        // so channel/user checks are neutered here by passing allow-all.
        let no_ids: HashSet<String> = HashSet::new();
        let filter = EventFilterParams {
            allow_all_channels: true,
            allowed_channels: &no_ids,
            allow_all_users: true,
            allowed_users: &no_ids,
            allow_bot_messages,
            trusted_bot_ids: &trusted_bot_ids,
            bot_username: bot_username.as_deref(),
        };
        let teams_event_context = Arc::new(GatewayEventContext {
            adapter: adapter.clone(),
            dispatcher: dispatcher.clone(),
            router: router.clone(),
            allow_bot_messages,
            trusted_bot_ids: trusted_bot_ids.clone(),
            bot_username: bot_username.clone(),
            stt_config: stt_config.clone(),
            teams_scope_policy: teams_scope_policy.clone(),
            teams_inbound_attachments,
            #[cfg(feature = "filestore")]
            filestore: filestore.clone(),
        });
        let teams_event_order = Arc::new(Mutex::new(()));

        loop {
            tokio::select! {
                    msg = ws_rx.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                let text_str: &str = &text;

                                if let Ok(envelope) = serde_json::from_str::<GatewayEnvelope>(text_str) {
                                    if envelope.schema == GATEWAY_HELLO_SCHEMA {
                                        match serde_json::from_str::<GatewayHello>(text_str) {
                                            Ok(hello)
                                                if hello.schema == GATEWAY_HELLO_SCHEMA
                                                    && hello.protocol_version == GATEWAY_PROTOCOL_VERSION => {
                                                if !hello.topology.supported {
                                                    warn!(
                                                        active_consumers = hello.topology.active_consumers,
                                                        delivery_mode = %hello.topology.delivery_mode,
                                                        "gateway reports unsupported multi-consumer topology"
                                                    );
                                                }
                                                info!(
                                                    protocol_version = hello.protocol_version,
                                                    capability_count = hello.capabilities.len(),
                                                    "gateway capabilities negotiated"
                                                );
                                                capability_state.update(hello);
                                            }
                                            Ok(hello) => {
                                                warn!(
                                                    peer_version = hello.protocol_version,
                                                    supported_version = GATEWAY_PROTOCOL_VERSION,
                                                    "gateway hello version is unsupported; continuing in legacy mode"
                                                );
                                            }
                                            Err(error) => {
                                                warn!(error = %error, "invalid gateway hello; continuing in legacy mode");
                                            }
                                        }
                                        continue;
                                    }
                                }

                                // Check if it's a response to a pending command
                                if let Ok(resp) = serde_json::from_str::<GatewayResponse>(text_str) {
                                    if resp.schema == "openab.gateway.response.v1" {
                                        if let Some(tx) = pending.lock().await.remove(&resp.request_id) {
                                            let _ = tx.send(resp);
                                        }
                                        continue;
                                    }
                                }

                            match serde_json::from_str::<GatewayEvent>(text_str) {
                                Ok(event) => {
                                    if event.platform.eq_ignore_ascii_case("teams")
                                        && !event.content.attachments.is_empty()
                                    {
                                        let event_json = text_str.to_owned();
                                        let event_context = teams_event_context.clone();
                                        let event_order = teams_event_order.clone();
                                        tasks.spawn(async move {
                                            let _guard = event_order.lock().await;
                                            if let Err(error) =
                                                process_gateway_event(&event_json, &event_context).await
                                            {
                                                warn!(error = %error, "teams attachment event processing failed");
                                            }
                                        });
                                        continue;
                                    }
                                    if should_skip_event(&event, &filter) {
                                        continue;
                                    }

                                    // Shared ingress trust gate (L2 scope + L3
                                    // identity) — same per-platform registry as
                                    // the unified path. Placed before slash
                                    // handling so untrusted senders cannot
                                    // execute /reset|/cancel|/config. The echo
                                    // is SPAWNED, never awaited here: streaming
                                    // send_message waits for a GatewayResponse
                                    // that only this loop can dispatch, so an
                                    // inline await would stall all event
                                    // processing for the reply timeout.
                                    match gate_gateway_event(&router, &event, &teams_scope_policy) {
                                        GateOutcome::Allow => {}
                                        GateOutcome::Deny { echo } => {
                                            if let Some((echo_channel, msg)) = echo {
                                                let echo_adapter = adapter.clone();
                                                tasks.spawn(async move {
                                                    let _ = echo_adapter
                                                        .send_message(&echo_channel, &msg)
                                                        .await;
                                                });
                                            }
                                            continue;
                                        }
                                    }

                                    let prompt = strip_recipient_mention(&event);

                                    info!(
                                        platform = %event.platform,
                                        sender = %event.sender.name,
                                        channel = %redact_channel(&event.channel.id),
                                        "gateway event received"
                                    );

                                    let channel = ChannelRef {
                                        platform: event.platform.clone(),
                                        channel_id: event.channel.id.clone(),
                                        thread_id: event.channel.thread_id.clone(),
                                        parent_id: None,
                                        origin_event_id: Some(event.event_id.clone()),
                                    };

                                    let sender_ctx = SenderContext {
                                        schema: "openab.sender.v1".into(),
                                        sender_id: event.sender.id.clone(),
                                        sender_name: event.sender.name.clone(),
                                        display_name: event.sender.display_name.clone(),
                                        channel: event.channel.channel_type.clone(),
                                        channel_id: event.channel.id.clone(),
                                        thread_id: event.channel.thread_id.clone(),
                                        is_bot: event.sender.is_bot,
                                        // Gateway: use event timestamp if available, else broker receive time
                                        timestamp: Some(if event.timestamp.is_empty() {
                                            crate::timestamp::now_iso8601()
                                        } else {
                                            event.timestamp.clone()
                                        }),
                                        message_id: if event.message_id.is_empty() { None } else { Some(event.message_id.clone()) },
                                        receiver_id: event.recipient.as_ref().map(|recipient| recipient.id.clone()),
                                    };
                                    let sender_json = serde_json::to_string(&sender_ctx)
                                        .unwrap_or_default();

                                    let trigger_msg = MessageRef {
                                        channel: channel.clone(),
                                        message_id: event.message_id.clone(),
                                    };

                                    let adapter = adapter.clone();
                                    let sender_name = event.sender.name.clone();
                                    let sender_id = event.sender.id.clone();
                                    let dispatcher = dispatcher.clone();

                                    // Convert gateway attachments to ContentBlocks
                                    let mut extra_blocks = Vec::new();
                                    for att in &event.content.attachments {
                                        // Rejected/truncated attachment: surface reason to the agent and skip.
                                        if let Some(ref reason) = att.status {
                                            tracing::info!(
                                                filename = %att.filename,
                                                mime_type = %att.mime_type,
                                                size = att.size,
                                                reason = %reason,
                                                "gateway attachment rejected, forwarding reason to agent"
                                            );
                                            let size_str = {
                                                let n = att.size;
                                                if n >= 1024 * 1024 {
                                                    format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
                                                } else if n >= 1024 {
                                                    format!("{:.1} KB", n as f64 / 1024.0)
                                                } else {
                                                    format!("{} B", n)
                                                }
                                            };
                                            extra_blocks.push(ContentBlock::Text {
                                                text: format!(
                                                    "[System: attachment \"{}\" ({}, {}) was not delivered — {}]",
                                                    att.filename, att.mime_type, size_str, reason
                                                ),
                                            });
                                            continue;
                                        }

                                        // Read bytes: prefer file path (colocate), fallback to base64
                                        let bytes_result = if let Some(ref path) = att.path {
                                            tokio::fs::read(path).await.map_err(|e| e.to_string())
                                        } else if !att.data.is_empty() {
                                            use base64::Engine;
                                            base64::engine::general_purpose::STANDARD
                                                .decode(&att.data)
                                                .map_err(|e| e.to_string())
                                        } else {
                                            tracing::warn!(
                                                filename = %att.filename,
                                                mime = %att.mime_type,
                                                "gateway: attachment has no path or data, skipping"
                                            );
                                            Err("no path or data".into())
                                        };

                                        match att.attachment_type.as_str() {
                                            "image" => {
                                                match bytes_result {
                                                    Ok(bytes) => {
                                                        use base64::Engine;
                                                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                                        extra_blocks.push(ContentBlock::Image {
                                                            media_type: att.mime_type.clone(),
                                                            data: b64,
                                                        });
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(filename = %att.filename, error = %e, "gateway image read failed");
                                                    }
                                                }
                                            }
                                            "text_file" => {
                                                if let Ok(bytes) = bytes_result {
                                                    let safe_filename: String = att.filename
                                                        .chars()
                                                        .filter(|c| !c.is_control())
                                                        .take(200)
                                                        .collect();
                                                    let size = bytes.len() as u64;
                                                    if size <= crate::media::TEXT_INLINE_LIMIT {
                                                        let text = String::from_utf8_lossy(&bytes);
                                                        extra_blocks.push(ContentBlock::Text {
                                                            text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                                                        });
                                                    } else {
                                                        // Large file — upload to filestore if available
                                                        #[cfg(feature = "filestore")]
                                                        if let Some(ref fs) = filestore {
                                                            if let Some((block, _)) = crate::media::upload_bytes_to_filestore_public(&att.filename, &bytes, fs).await {
                                                                extra_blocks.push(block);
                                                            } else {
                                                                // Upload refused (size cap) — emit degraded hint, don't inline oversized body
                                                                let size_kb = bytes.len() / 1024;
                                                                tracing::warn!(filename = %att.filename, size = bytes.len(), "filestore upload refused; emitting degraded hint");
                                                                extra_blocks.push(ContentBlock::Text {
                                                                    text: format!(
                                                                        "[File: {safe_filename}]\nThis file ({size_kb} KB) exceeds the configured upload limit and could not be stored."
                                                                    ),
                                                                });
                                                            }
                                                        } else {
                                                            // No filestore configured — fall back to inline (original behavior)
                                                            let text = String::from_utf8_lossy(&bytes);
                                                            extra_blocks.push(ContentBlock::Text {
                                                                text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                                                            });
                                                        }
                                                        #[cfg(not(feature = "filestore"))]
                                                        {
                                                            // Feature not compiled — inline as before
                                                            let text = String::from_utf8_lossy(&bytes);
                                                            extra_blocks.push(ContentBlock::Text {
                                                                text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                            "audio" if stt_config.enabled => {
                                                match bytes_result {
                                                    Ok(bytes) => {
                                                        match crate::stt::transcribe(
                                                            &crate::media::HTTP_CLIENT,
                                                            &stt_config,
                                                            bytes,
                                                            att.filename.clone(),
                                                            &att.mime_type,
                                                        ).await {
                                                            Some(transcript) => {
                                                                extra_blocks.push(ContentBlock::Text {
                                                                    text: format!("[Voice message transcript]: {transcript}"),
                                                                });
                                                            }
                                                            None => {
                                                                tracing::warn!(filename = %att.filename, "gateway audio STT failed");
                                                                extra_blocks.push(ContentBlock::Text {
                                                                    text: format!(
                                                                        "[Voice message — transcription failed for {}]",
                                                                        att.filename
                                                                    ),
                                                                });
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(filename = %att.filename, error = %e, "gateway audio read failed");
                                                        extra_blocks.push(ContentBlock::Text {
                                                            text: format!(
                                                                "[Voice message — read failed for {}]",
                                                                att.filename
                                                            ),
                                                        });
                                                    }
                                                }
                                            }
                                            "audio" => {
                                                tracing::debug!(filename = %att.filename, "audio attachment skipped — STT not enabled");
                                            }
                                            _ => {}
                                        }
                                    }

                                    if prompt.is_empty() && extra_blocks.is_empty() {
                                        continue;
                                    }

                                    // Slash command interception for gateway platforms
                                    // (Feishu/LINE/Telegram don't have native slash commands)
                                    // Use fire-and-forget send — slash command responses don't
                                    // need message_id for streaming edits.
                                    let trimmed = prompt.trim();
                                    if trimmed == "/reset" {
                                        let thread_id_str = event.channel.thread_id.as_deref().unwrap_or(&event.channel.id);
                                        let thread_key = format!("{}:{}", event.platform, thread_id_str);
                                        let dropped = dispatcher.cancel_buffered_thread(event.platform.as_str(), thread_id_str);
                                        let msg = match (router.pool().reset_session(&thread_key).await, dropped) {
                                            (Ok(()), 0) => "🔄 Session reset. Start a new conversation!".to_string(),
                                            (Ok(()), n) => format!("🔄 Session reset. Dropped {n} buffered message(s). Start a new conversation!"),
                                            (Err(_), 0) => "⚠️ No active session to reset.".to_string(),
                                            (Err(_), n) => format!("🔄 Dropped {n} buffered message(s). No active session to reset."),
                                        };
                                        let _ = send_fire_and_forget(&slash_ws_tx, &channel, &msg).await;
                                        continue;
                                    }
                                    if trimmed == "/cancel" {
                                        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
                                        let msg = match router.pool().cancel_session(&thread_key).await {
                                            Ok(()) => "🛑 Cancel signal sent.".to_string(),
                                            Err(e) => format!("⚠️ {e}"),
                                        };
                                        let _ = send_fire_and_forget(&slash_ws_tx, &channel, &msg).await;
                                        continue;
                                    }
                                    {
                                        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
                                        if let Some(msg) = handle_config_command(trimmed, &router, &thread_key).await {
                                            let _ = send_fire_and_forget(&slash_ws_tx, &channel, &msg).await;
                                            continue;
                                        }
                                    }

                                    tasks.spawn(async move {
                                        // If supergroup with no thread_id, create a forum topic
                                        let thread_channel = if event.channel.channel_type == "supergroup"
                                            && channel.thread_id.is_none()
                                        {
                                            let title = crate::format::shorten_thread_name(&prompt);
                                            match adapter.create_thread(&channel, &trigger_msg, &title).await {
                                                Ok(tc) => tc,
                                                Err(e) => {
                                                    warn!("create_thread failed, replying in channel: {e}");
                                                    channel.clone()
                                                }
                                            }
                                        } else {
                                            channel.clone()
                                        };

                                        let thread_id = thread_channel
                                            .thread_id
                                            .as_deref()
                                            .unwrap_or(&thread_channel.channel_id);
                                        let thread_key = dispatcher.key(
                                            &thread_channel.platform,
                                            thread_id,
                                            &sender_id,
                                        );
                                        let estimated_tokens =
                                            crate::dispatch::estimate_tokens(&prompt, &extra_blocks);
                                        let buf_msg = crate::dispatch::BufferedMessage {
                                            sender_json,
                                            sender_name,
                                            prompt,
                                            extra_blocks,
                                            trigger_msg,
                                            arrived_at: std::time::Instant::now(),
                                            estimated_tokens,
                                            // TODO: implement gateway multibot detection
                                            other_bot_present: false,
                                            recipient: None, // Slack-only (assistant mode); N/A for gateway
                                        };
                                        if let Err(e) = dispatcher
                                            .submit(thread_key, thread_channel, adapter, buf_msg)
                                            .await
                                        {
                                            error!("gateway dispatcher submit error: {e}");
                                        }
                                    });
                                }
                                Err(e) => warn!("invalid gateway event: {e}"),
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            warn!("gateway WebSocket closed, will reconnect");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("gateway WebSocket error: {e}, will reconnect");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        connection_active.store(false, AtomicOrdering::Release);
                        pending.lock().await.clear();
                        info!("gateway adapter shutting down, waiting for {} in-flight tasks", tasks.len());
                        while tasks.join_next().await.is_some() {}
                        return Ok(());
                    }
                }
            }
        } // inner loop — break here means reconnect

        // Stop new attachment commands and wake any request waiting on a
        // response that cannot arrive on this connection. Queued event tasks
        // then fail materialization immediately and can still dispatch text.
        connection_active.store(false, AtomicOrdering::Release);
        pending.lock().await.clear();

        // Drain in-flight tasks before reconnecting
        while tasks.join_next().await.is_some() {}

        warn!(backoff = backoff_secs, "reconnecting to gateway");
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => { return Ok(()); }
        }
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
    } // outer reconnect loop
}

// --- Public API for unified mode (Phase 2) ---

/// Context required to process a gateway event without a WebSocket connection.
/// Used by the unified binary to dispatch webhook events directly.
#[derive(Clone)]
pub struct GatewayEventContext {
    pub adapter: Arc<dyn ChatAdapter>,
    pub dispatcher: Arc<crate::dispatch::Dispatcher>,
    pub router: Arc<crate::adapter::AdapterRouter>,
    pub allow_bot_messages: bool,
    pub trusted_bot_ids: HashSet<String>,
    pub bot_username: Option<String>,
    pub stt_config: crate::config::SttConfig,
    pub teams_scope_policy: TeamsScopePolicy,
    pub teams_inbound_attachments: bool,
    #[cfg(feature = "filestore")]
    pub filestore: Option<Arc<crate::filestore::Filestore>>,
}

/// Process a single gateway event JSON string and submit to the dispatcher.
/// Returns Ok(true) if the event was dispatched, Ok(false) if filtered/skipped,
/// or Err if the JSON is invalid.
///
/// This is the core event-handling logic extracted from the WebSocket handler,
/// made available for the unified binary to call directly from axum webhook handlers.
/// Throttle for request-access echoes: at most one echo per (platform, sender)
/// per [`ECHO_WINDOW`], to prevent an untrusted spammer from being amplified by
/// the bot's replies.
static ECHO_THROTTLE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

const ECHO_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// Returns true if an echo to `key` is allowed now (and records the timestamp).
fn echo_allowed(key: &str) -> bool {
    let now = std::time::Instant::now();
    let mut map = ECHO_THROTTLE.lock().unwrap();
    match map.get(key) {
        Some(prev) if now.duration_since(*prev) < ECHO_WINDOW => false,
        _ => {
            map.insert(key.to_string(), now);
            true
        }
    }
}

/// Outcome of the shared ingress trust gate. `Deny { echo }` carries the
/// throttled request-access echo payload (channel + message) when the deny is
/// an identity deny and the per-sender throttle admits it; the CALLER decides
/// how to deliver it. Delivery must NOT be awaited inline inside the WS event
/// loop: `GatewayAdapter::send_message` (streaming mode) waits for a
/// `GatewayResponse` that is dispatched by that same loop — awaiting there
/// would stall all event processing for the reply timeout. The WS path spawns
/// the echo; the unified path (axum/bridge task) awaits it directly.
enum GateOutcome {
    Allow,
    Deny { echo: Option<(ChannelRef, String)> },
}

/// Shared ingress trust gate for gateway events — used by BOTH the standalone
/// WebSocket path (`run_gateway_adapter`) and the unified path
/// (`process_gateway_event`), so L2 (channel scope) and L3 (identity) are
/// enforced by the same per-platform registry regardless of deployment mode
/// (#1356 Phase 1c prerequisite).
///
/// On `DenyIdentity`, returns the throttled request-access echo payload; on
/// `DenyScope` (and any future variant), denies silently — scope is not a
/// security boundary, so no echo.
///
/// Teams events carrying authenticated typed scope use the Teams-specific L2
/// policy and then the shared L3 identity gate. Events from old peers without
/// `scope` retain the legacy conversation-ID / `is_dm = false` behavior.
fn gate_gateway_event(
    router: &crate::adapter::AdapterRouter,
    event: &GatewayEvent,
    teams_scope_policy: &TeamsScopePolicy,
) -> GateOutcome {
    let decision = if event.platform.eq_ignore_ascii_case("teams") {
        match event.scope.as_ref() {
            Some(scope)
                if teams_scope_policy.surface_allowed(
                    &event.channel.id,
                    &event.channel.channel_type,
                    scope,
                ) =>
            {
                router.gate_identity(&event.platform, &event.sender.id)
            }
            Some(_) => crate::trust::Decision::DenyScope,
            None => {
                if !teams_scope_policy.uses_legacy_fallback() {
                    tracing::warn!(
                        "gateway: Teams event has no typed scope; using legacy conversation-ID \
                         fallback for rolling compatibility"
                    );
                }
                router.gate_incoming(&event.platform, &event.channel.id, false, &event.sender.id)
            }
        }
    } else {
        router.gate_incoming(&event.platform, &event.channel.id, false, &event.sender.id)
    };
    match decision {
        crate::trust::Decision::Allow => GateOutcome::Allow,
        crate::trust::Decision::DenyIdentity => {
            // L3 identity deny → echo the sender their ID so they can request
            // access (throttled to avoid amplification). Bots never reach here
            // (should_skip_event handles bot admission; L3 is human-only).
            tracing::info!(
                platform = %event.platform,
                sender = %event.sender.id,
                channel = %redact_channel(&event.channel.id),
                "gateway event denied (identity); echoing request-access"
            );
            let throttle_key = format!("{}:{}", event.platform, event.sender.id);
            let echo = if echo_allowed(&throttle_key) {
                let echo_channel = ChannelRef {
                    platform: event.platform.clone(),
                    channel_id: event.channel.id.clone(),
                    thread_id: event.channel.thread_id.clone(),
                    parent_id: None,
                    origin_event_id: Some(event.event_id.clone()),
                };
                let msg = format!(
                    "⚠️ You are not on this bot's trusted list.\nYour ID: {}\nAsk the admin to add it to allowed_users.",
                    event.sender.id
                );
                Some((echo_channel, msg))
            } else {
                None
            };
            GateOutcome::Deny { echo }
        }
        // DenyScope (and any future variant) → silent drop (scope is not a
        // security boundary; no request-access echo).
        _ => {
            tracing::info!(
                platform = %event.platform,
                sender = %event.sender.id,
                channel = %redact_channel(&event.channel.id),
                ?decision,
                "gateway event denied (scope); silent"
            );
            GateOutcome::Deny { echo: None }
        }
    }
}

pub async fn process_gateway_event(
    event_json: &str,
    ctx: &GatewayEventContext,
) -> anyhow::Result<bool> {
    let event: GatewayEvent = serde_json::from_str(event_json)
        .map_err(|e| anyhow::anyhow!("invalid gateway event JSON: {e}"))?;

    // Structural gating (bot filter + @mention) stays in should_skip_event.
    // L2 (channel) + L3 (identity) are now enforced by the shared ingress gate
    // (`router.gate_incoming`) below, so we neuter should_skip_event's channel/user
    // checks here by passing allow-all for them.
    let no_ids: HashSet<String> = HashSet::new();
    let filter = EventFilterParams {
        allow_all_channels: true,
        allowed_channels: &no_ids,
        allow_all_users: true,
        allowed_users: &no_ids,
        allow_bot_messages: ctx.allow_bot_messages,
        trusted_bot_ids: &ctx.trusted_bot_ids,
        bot_username: ctx.bot_username.as_deref(),
    };
    if should_skip_event(&event, &filter) {
        return Ok(false);
    }

    // Shared ingress trust gate (L2 scope + L3 identity), keyed by platform.
    // Awaiting echo delivery here is safe: this runs on the axum/bridge task,
    // not inside the WS event loop.
    match gate_gateway_event(&ctx.router, &event, &ctx.teams_scope_policy) {
        GateOutcome::Allow => {}
        GateOutcome::Deny { echo } => {
            if let Some((echo_channel, msg)) = echo {
                let _ = ctx.adapter.send_message(&echo_channel, &msg).await;
            }
            return Ok(false);
        }
    }

    let prompt = strip_recipient_mention(&event);

    tracing::info!(
        platform = %event.platform,
        sender = %event.sender.name,
        channel = %redact_channel(&event.channel.id),
        "gateway event received (unified)"
    );

    let channel = ChannelRef {
        platform: event.platform.clone(),
        channel_id: event.channel.id.clone(),
        thread_id: event.channel.thread_id.clone(),
        parent_id: None,
        origin_event_id: Some(event.event_id.clone()),
    };

    let sender_ctx = SenderContext {
        schema: "openab.sender.v1".into(),
        sender_id: event.sender.id.clone(),
        sender_name: event.sender.name.clone(),
        display_name: event.sender.display_name.clone(),
        channel: event.channel.channel_type.clone(),
        channel_id: event.channel.id.clone(),
        thread_id: event.channel.thread_id.clone(),
        is_bot: event.sender.is_bot,
        timestamp: Some(if event.timestamp.is_empty() {
            crate::timestamp::now_iso8601()
        } else {
            event.timestamp.clone()
        }),
        message_id: if event.message_id.is_empty() { None } else { Some(event.message_id.clone()) },
        receiver_id: event
            .recipient
            .as_ref()
            .map(|recipient| recipient.id.clone()),
    };
    let sender_json = serde_json::to_string(&sender_ctx).unwrap_or_default();

    let trigger_msg = MessageRef {
        channel: channel.clone(),
        message_id: event.message_id.clone(),
    };

    // Convert gateway attachments to ContentBlocks. Teams references are
    // resolved only here, after the authoritative structural + L2 + L3 gate.
    let mut extra_blocks = Vec::new();
    let teams_event = event.platform.eq_ignore_ascii_case("teams");
    let attachment_limit = if teams_event { 10 } else { usize::MAX };
    let attachment_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    let attachment_capabilities = ctx.adapter.capabilities(&event.platform);
    for metadata in event.content.attachments.iter().take(attachment_limit) {
        if teams_event && !ctx.teams_inbound_attachments {
            continue;
        }
        let mut att = metadata.clone();
        let mut materialized_data = None;
        if teams_event
            && (att.attachment_type.len() > 32
                || att.attachment_type.chars().any(char::is_control)
                || att.filename.chars().count() > 200
                || att.filename.chars().any(char::is_control)
                || att.mime_type.len() > 128
                || att.mime_type.chars().any(char::is_control)
                || att.status.as_ref().is_some_and(|status| {
                    status.len() > 256 || status.chars().any(char::is_control)
                }))
        {
            continue;
        }
        let reference = att.reference.take();
        // Teams never accepts pre-materialized bytes or a Gateway-local path.
        // A no-reference entry is usable only as bounded rejected metadata.
        if teams_event && reference.is_none() && att.status.is_none() {
            continue;
        }
        if let Some(reference) = reference {
            if !teams_event || !attachment_capabilities.supports_attachment_materialization {
                continue;
            }
            if tokio::time::Instant::now() >= attachment_deadline {
                att.status =
                    Some("download failed: attachment materialization batch timed out".into());
            } else {
                match tokio::time::timeout_at(
                    attachment_deadline,
                    ctx.adapter.materialize_attachment(&channel, &reference),
                )
                .await
                {
                    Ok(Ok(materialized)) => {
                        att.attachment_type = materialized.attachment_type;
                        att.filename = materialized.filename;
                        att.mime_type = materialized.mime_type;
                        att.size = materialized.size;
                        att.path = None;
                        att.data.clear();
                        att.status = materialized.status;
                        if att.status.is_none()
                            && att.attachment_type == "text_file"
                            && std::str::from_utf8(&materialized.data).is_err()
                        {
                            att.status =
                                Some("invalid content: text attachment is not valid UTF-8".into());
                        } else {
                            materialized_data = Some(materialized.data);
                        }
                    }
                    Ok(Err(_)) => {
                        att.status =
                            Some("download failed: attachment materialization failed".into());
                    }
                    Err(_) => {
                        att.status = Some(
                            "download failed: attachment materialization batch timed out".into(),
                        );
                    }
                }
            }
        }

        if let Some(ref reason) = att.status {
            let size_str = format_size(att.size);
            extra_blocks.push(ContentBlock::Text {
                text: format!(
                    "[System: attachment \"{}\" ({}, {}) was not delivered — {}]",
                    att.filename, att.mime_type, size_str, reason
                ),
            });
            continue;
        }

        let bytes_result = if let Some(bytes) = materialized_data {
            Ok(bytes)
        } else if let Some(ref path) = att.path {
            tokio::fs::read(path).await.map_err(|e| e.to_string())
        } else if !att.data.is_empty() {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(&att.data)
                .map_err(|e| e.to_string())
        } else {
            tracing::warn!(
                filename = %att.filename,
                mime = %att.mime_type,
                "gateway: attachment has no path or data, skipping"
            );
            Err("no path or data".into())
        };

        match att.attachment_type.as_str() {
            "image" => {
                match bytes_result {
                    Ok(bytes) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        extra_blocks.push(ContentBlock::Image {
                            media_type: att.mime_type.clone(),
                            data: b64,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(filename = %att.filename, error = %e, "gateway image read failed");
                    }
                }
            }
            "text_file" => {
                match bytes_result {
                    Ok(bytes) => {
                        let safe_filename: String = att.filename
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(200)
                            .collect();
                        let size = bytes.len() as u64;
                        if size <= crate::media::TEXT_INLINE_LIMIT {
                            let text = String::from_utf8_lossy(&bytes);
                            extra_blocks.push(ContentBlock::Text {
                                text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                            });
                        } else {
                            // Large file — upload to filestore if available
                            #[cfg(feature = "filestore")]
                            if let Some(ref fs) = ctx.filestore {
                                if let Some((block, _)) = crate::media::upload_bytes_to_filestore_public(&att.filename, &bytes, fs).await {
                                    extra_blocks.push(block);
                                } else {
                                    // Upload refused (size cap) — emit degraded hint, don't inline oversized body
                                    let size_kb = bytes.len() / 1024;
                                    tracing::warn!(filename = %att.filename, size = bytes.len(), "filestore upload refused; emitting degraded hint");
                                    extra_blocks.push(ContentBlock::Text {
                                        text: format!(
                                            "[File: {safe_filename}]\nThis file ({size_kb} KB) exceeds the configured upload limit and could not be stored."
                                        ),
                                    });
                                }
                            } else {
                                // No filestore configured — fall back to inline (original behavior)
                                let text = String::from_utf8_lossy(&bytes);
                                extra_blocks.push(ContentBlock::Text {
                                    text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                                });
                            }
                            #[cfg(not(feature = "filestore"))]
                            {
                                // Feature not compiled — inline as before
                                let text = String::from_utf8_lossy(&bytes);
                                extra_blocks.push(ContentBlock::Text {
                                    text: format!("[File: {safe_filename}]\n```\n{text}\n```"),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(filename = %att.filename, error = %e, "gateway text_file read failed");
                    }
                }
            }
            "audio" if ctx.stt_config.enabled => {
                match bytes_result {
                    Ok(bytes) => {
                        match crate::stt::transcribe(
                            &crate::media::HTTP_CLIENT,
                            &ctx.stt_config,
                            bytes,
                            att.filename.clone(),
                            &att.mime_type,
                        ).await {
                            Some(transcript) => {
                                extra_blocks.push(ContentBlock::Text {
                                    text: format!("[Voice message transcript]: {transcript}"),
                                });
                            }
                            None => {
                                extra_blocks.push(ContentBlock::Text {
                                    text: format!("[Voice message — transcription failed for {}]", att.filename),
                                });
                            }
                        }
                    }
                    Err(_) => {
                        extra_blocks.push(ContentBlock::Text {
                            text: format!("[Voice message — read failed for {}]", att.filename),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if prompt.is_empty() && extra_blocks.is_empty() {
        return Ok(false);
    }

    // Slash command interception
    let trimmed = prompt.trim();
    if trimmed == "/reset" {
        let thread_id_str = event.channel.thread_id.as_deref().unwrap_or(&event.channel.id);
        let thread_key = format!("{}:{}", event.platform, thread_id_str);
        let dropped = ctx.dispatcher.cancel_buffered_thread(event.platform.as_str(), thread_id_str);
        let msg = match (ctx.router.pool().reset_session(&thread_key).await, dropped) {
            (Ok(()), 0) => "🔄 Session reset. Start a new conversation!".to_string(),
            (Ok(()), n) => format!("🔄 Session reset. Dropped {n} buffered message(s). Start a new conversation!"),
            (Err(_), 0) => "⚠️ No active session to reset.".to_string(),
            (Err(_), n) => format!("🔄 Dropped {n} buffered message(s). No active session to reset."),
        };
        let _ = ctx.adapter.send_message(&channel, &msg).await;
        return Ok(false);
    }
    if trimmed == "/cancel" {
        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
        let msg = match ctx.router.pool().cancel_session(&thread_key).await {
            Ok(()) => "🛑 Cancel signal sent.".to_string(),
            Err(e) => format!("⚠️ {e}"),
        };
        let _ = ctx.adapter.send_message(&channel, &msg).await;
        return Ok(false);
    }
    {
        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
        if let Some(msg) = handle_config_command(trimmed, &ctx.router, &thread_key).await {
            let _ = ctx.adapter.send_message(&channel, &msg).await;
            return Ok(false);
        }
    }

    // Submit to dispatcher
    let adapter = ctx.adapter.clone();
    let dispatcher = ctx.dispatcher.clone();
    let sender_name = event.sender.name.clone();
    let sender_id = event.sender.id.clone();

    tokio::spawn(async move {
        let thread_channel = if event.channel.channel_type == "supergroup"
            && channel.thread_id.is_none()
        {
            let title = crate::format::shorten_thread_name(&prompt);
            match adapter.create_thread(&channel, &trigger_msg, &title).await {
                Ok(tc) => tc,
                Err(e) => {
                    tracing::warn!("create_thread failed, replying in channel: {e}");
                    channel.clone()
                }
            }
        } else {
            channel.clone()
        };

        let thread_id = thread_channel
            .thread_id
            .as_deref()
            .unwrap_or(&thread_channel.channel_id);
        let thread_key = dispatcher.key(
            &thread_channel.platform,
            thread_id,
            &sender_id,
        );
        let estimated_tokens =
            crate::dispatch::estimate_tokens(&prompt, &extra_blocks);
        let buf_msg = crate::dispatch::BufferedMessage {
            sender_json,
            sender_name,
            prompt,
            extra_blocks,
            trigger_msg,
            arrived_at: std::time::Instant::now(),
            estimated_tokens,
            other_bot_present: false,
            recipient: None,
        };
        if let Err(e) = dispatcher
            .submit(thread_key, thread_channel, adapter, buf_msg)
            .await
        {
            tracing::error!("gateway dispatcher submit error: {e}");
        }
    });

    Ok(true)
}

fn format_size(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{} B", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AttachmentProbeAdapter {
        materializations: AtomicUsize,
        sends: AtomicUsize,
    }

    #[async_trait]
    impl ChatAdapter for AttachmentProbeAdapter {
        fn platform(&self) -> &'static str {
            "probe"
        }

        fn message_limit(&self) -> usize {
            4096
        }

        fn capabilities(&self, platform: &str) -> AdapterCapabilities {
            AdapterCapabilities {
                supports_attachment_materialization: platform == "teams",
                ..AdapterCapabilities::default()
            }
        }

        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }

        async fn materialize_attachment(
            &self,
            _channel: &ChannelRef,
            _reference: &str,
        ) -> Result<MaterializedAttachment> {
            self.materializations.fetch_add(1, Ordering::SeqCst);
            Ok(MaterializedAttachment {
                attachment_type: "text_file".into(),
                filename: "notes.txt".into(),
                mime_type: "text/plain; charset=utf-8".into(),
                data: b"secret bytes".to_vec(),
                size: 12,
                status: None,
            })
        }

        async fn send_message(&self, channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: "echo".into(),
            })
        }

        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger_msg: &MessageRef,
            _title: &str,
        ) -> Result<ChannelRef> {
            Ok(channel.clone())
        }

        async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }
    }

    fn attachment_event_json(sender_id: &str) -> String {
        serde_json::json!({
            "schema": "openab.gateway.event.v1",
            "event_id": "event-attachment",
            "timestamp": "",
            "platform": "teams",
            "event_type": "message",
            "channel": {
                "id": "conversation-1",
                "type": "personal",
                "thread_id": null
            },
            "sender": {
                "id": sender_id,
                "name": "Attachment User",
                "display_name": "Attachment User",
                "is_bot": false
            },
            "content": {
                "type": "text",
                "text": "",
                "attachments": [{
                    "type": "text_file",
                    "filename": "notes.txt",
                    "mime_type": "text/plain",
                    "reference": "att-opaque",
                    "data": "",
                    "path": null,
                    "size": 0,
                    "status": null
                }]
            },
            "mentions": [],
            "message_id": "activity-1",
            "scope": {
                "tenant_id": "tenant-1",
                "team_id": null,
                "channel_id": null,
                "conversation_type": "personal",
                "trust_scope_id": "teams:tenant-1:personal:conversation-1",
                "is_dm": true
            },
            "recipient": null,
            "mention_entities": []
        })
        .to_string()
    }

    #[tokio::test]
    async fn identity_denial_precedes_attachment_materialization() -> anyhow::Result<()> {
        let pool = Arc::new(crate::acp::SessionPool::new(
            crate::config::AgentConfig::default(),
            1,
            900,
            HashMap::new(),
        ));
        let router = Arc::new(AdapterRouter::new(
            pool,
            crate::config::ReactionsConfig::default(),
            crate::markdown::TableMode::default(),
            900,
            30,
            HashMap::new(),
            std::env::temp_dir(),
        ));
        let dispatcher = Arc::new(crate::dispatch::Dispatcher::with_idle_timeout(
            router.clone(),
            1,
            24_000,
            crate::dispatch::BatchGrouping::Thread,
            std::time::Duration::from_secs(1),
        ));
        let probe = Arc::new(AttachmentProbeAdapter {
            materializations: AtomicUsize::new(0),
            sends: AtomicUsize::new(0),
        });
        let adapter: Arc<dyn ChatAdapter> = probe.clone();
        let context = GatewayEventContext {
            adapter,
            dispatcher,
            router,
            allow_bot_messages: false,
            trusted_bot_ids: HashSet::new(),
            bot_username: None,
            stt_config: crate::config::SttConfig::default(),
            teams_scope_policy: TeamsScopePolicy::default(),
            teams_inbound_attachments: true,
            #[cfg(feature = "filestore")]
            filestore: None,
        };
        let event_json = attachment_event_json("untrusted-user");
        assert!(!process_gateway_event(&event_json, &context).await?);
        assert_eq!(probe.materializations.load(Ordering::SeqCst), 0);
        assert_eq!(probe.sends.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn admitted_attachment_is_materialized_before_dispatch() -> anyhow::Result<()> {
        let router = Arc::new(teams_router(vec!["trusted-user".into()]));
        let dispatcher = Arc::new(crate::dispatch::Dispatcher::with_idle_timeout(
            router.clone(),
            1,
            24_000,
            crate::dispatch::BatchGrouping::Thread,
            std::time::Duration::from_secs(60),
        ));
        let probe = Arc::new(AttachmentProbeAdapter {
            materializations: AtomicUsize::new(0),
            sends: AtomicUsize::new(0),
        });
        let mut context = GatewayEventContext {
            adapter: probe.clone(),
            dispatcher,
            router,
            allow_bot_messages: false,
            trusted_bot_ids: HashSet::new(),
            bot_username: None,
            stt_config: crate::config::SttConfig::default(),
            teams_scope_policy: TeamsScopePolicy::default(),
            teams_inbound_attachments: true,
            #[cfg(feature = "filestore")]
            filestore: None,
        };

        assert!(process_gateway_event(
            &attachment_event_json("trusted-user"),
            &context,
        )
        .await?);
        assert_eq!(probe.materializations.load(Ordering::SeqCst), 1);
        assert_eq!(probe.sends.load(Ordering::SeqCst), 0);

        let mut injected: serde_json::Value =
            serde_json::from_str(&attachment_event_json("trusted-user"))?;
        injected["event_id"] = "event-pre-materialized".into();
        injected["content"]["attachments"][0]["reference"] = serde_json::Value::Null;
        injected["content"]["attachments"][0]["data"] = "c2VjcmV0".into();
        assert!(!process_gateway_event(&injected.to_string(), &context).await?);
        assert_eq!(probe.materializations.load(Ordering::SeqCst), 1);

        context.teams_inbound_attachments = false;
        assert!(!process_gateway_event(
            &attachment_event_json("trusted-user"),
            &context,
        )
        .await?);
        assert_eq!(probe.materializations.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn legacy_non_editable_platforms_are_send_once() {
        for platform in ["line", "lineworks", "acp"] {
            let capabilities = legacy_gateway_capabilities(platform, true, true);
            assert!(!capabilities.can_edit, "{platform} must not advertise edit");
            assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        }
    }

    #[test]
    fn legacy_editable_platforms_preserve_configured_streaming() {
        for platform in [
            "telegram",
            "slack",
            "discord",
            "feishu",
            "googlechat",
            "wecom",
        ] {
            let capabilities = legacy_gateway_capabilities(platform, true, true);
            assert!(capabilities.can_edit, "{platform} should advertise edit");
            assert_eq!(capabilities.streaming_mode, StreamingMode::Edit);
        }
        let feishu = legacy_gateway_capabilities("feishu", true, true);
        assert!(feishu.edit_ack);
        assert!(!feishu.send_ack, "legacy peers never require send ACK");
    }

    #[test]
    fn teams_processing_message_requires_all_negotiated_write_primitives() {
        let supported = AdapterCapabilities {
            send_ack: true,
            edit_ack: true,
            delete_ack: true,
            supports_target_message_id: true,
            supports_reactions: true,
            can_edit: true,
            can_delete: true,
            status_backend: StatusBackend::Reactions,
            ..AdapterCapabilities::default()
        };

        let mut legacy_reactions = AdapterCapabilities {
            status_backend: StatusBackend::Reactions,
            ..AdapterCapabilities::default()
        };
        normalize_reaction_support(&mut legacy_reactions);
        assert!(legacy_reactions.supports_reactions);

        let mut before_hello = supported.clone();
        apply_teams_processing_indicator(false, true, &mut before_hello);
        assert_eq!(before_hello.status_backend, StatusBackend::None);
        assert!(before_hello.supports_reactions);

        let mut disabled = supported.clone();
        apply_teams_processing_indicator(true, false, &mut disabled);
        assert_eq!(disabled.status_backend, StatusBackend::Reactions);

        for missing in 0..6 {
            let mut capabilities = supported.clone();
            match missing {
                0 => capabilities.send_ack = false,
                1 => capabilities.edit_ack = false,
                2 => capabilities.delete_ack = false,
                3 => capabilities.supports_target_message_id = false,
                4 => capabilities.can_edit = false,
                5 => capabilities.can_delete = false,
                _ => unreachable!(),
            }
            apply_teams_processing_indicator(true, true, &mut capabilities);
            assert_eq!(capabilities.status_backend, StatusBackend::None);
        }

        let mut capabilities = supported;
        apply_teams_processing_indicator(true, true, &mut capabilities);
        assert_eq!(capabilities.status_backend, StatusBackend::Message);
        assert!(capabilities.supports_reactions);
    }

    #[test]
    fn teams_progressive_response_requires_all_negotiated_write_primitives() {
        let supported = AdapterCapabilities {
            send_ack: true,
            edit_ack: true,
            delete_ack: true,
            supports_target_message_id: true,
            can_edit: true,
            can_delete: true,
            show_streaming_placeholder: true,
            ..AdapterCapabilities::default()
        };

        let mut before_hello = supported.clone();
        before_hello.streaming_mode = StreamingMode::Edit;
        apply_teams_progressive_capabilities(false, true, &mut before_hello);
        assert_eq!(before_hello.streaming_mode, StreamingMode::Disabled);

        let mut disabled = supported.clone();
        apply_teams_progressive_capabilities(true, false, &mut disabled);
        assert_eq!(disabled.streaming_mode, StreamingMode::Disabled);

        for missing in 0..7 {
            let mut capabilities = supported.clone();
            match missing {
                0 => capabilities.send_ack = false,
                1 => capabilities.edit_ack = false,
                2 => capabilities.delete_ack = false,
                3 => capabilities.supports_target_message_id = false,
                4 => capabilities.can_edit = false,
                5 => capabilities.can_delete = false,
                6 => capabilities.show_streaming_placeholder = false,
                _ => unreachable!(),
            }
            apply_teams_progressive_capabilities(true, true, &mut capabilities);
            assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        }

        let mut capabilities = supported;
        apply_teams_progressive_capabilities(true, true, &mut capabilities);
        assert_eq!(capabilities.streaming_mode, StreamingMode::Edit);
        assert!(capabilities.show_streaming_placeholder);
    }

    #[test]
    fn capability_state_uses_legacy_only_before_successful_negotiation() {
        let state = GatewayCapabilityState::default();
        let legacy = legacy_gateway_capabilities("telegram", true, true);
        let (negotiated, resolved) = state.resolve("telegram", &legacy);
        assert!(!negotiated);
        assert_eq!(resolved, legacy);

        let advertised = AdapterCapabilities {
            send_ack: true,
            can_edit: true,
            streaming_mode: StreamingMode::Edit,
            status_backend: StatusBackend::Reactions,
            ..AdapterCapabilities::default()
        };
        state.update(GatewayHello {
            schema: GATEWAY_HELLO_SCHEMA.into(),
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            capabilities: HashMap::from([("telegram".into(), advertised.clone())]),
            topology: GatewayTopology {
                active_consumers: 1,
                supported: true,
                delivery_mode: "best_effort_broadcast".into(),
            },
        });

        let (negotiated, resolved) = state.resolve("telegram", &legacy);
        assert!(negotiated);
        assert_eq!(resolved, advertised);

        // Once a hello was accepted, an omitted platform is not allowed to
        // inherit optimistic legacy behavior.
        let (_, missing) = state.resolve("unadvertised", &legacy);
        assert_eq!(missing, AdapterCapabilities::default());
        assert_eq!(missing.status_backend, StatusBackend::None);
    }

    #[test]
    fn legacy_and_structured_gateway_responses_map_to_write_outcomes() {
        let legacy: GatewayResponse = serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.response.v1",
            "request_id": "req-legacy",
            "success": true,
            "thread_id": null,
            "message_id": "activity-1",
            "error": null
        }))
        .unwrap();
        assert_eq!(
            legacy.write_outcome(),
            WriteOutcome::Delivered {
                message_id: Some("activity-1".into())
            }
        );

        let unknown: GatewayResponse = serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.response.v1",
            "request_id": "req-new",
            "success": false,
            "thread_id": null,
            "message_id": null,
            "error": "delivery may have completed",
            "outcome": "unknown",
            "error_code": "request_timeout"
        }))
        .unwrap();
        assert_eq!(
            unknown.write_outcome(),
            WriteOutcome::Unknown {
                code: "request_timeout".into(),
                message: "delivery may have completed".into()
            }
        );
    }

    #[test]
    fn command_target_field_is_negotiated_with_legacy_fallback() {
        let message = MessageRef {
            channel: ChannelRef {
                platform: "teams".into(),
                channel_id: "conversation-1".into(),
                thread_id: None,
                parent_id: None,
                origin_event_id: Some("event-1".into()),
            },
            message_id: "activity-1".into(),
        };
        let supported = AdapterCapabilities {
            supports_target_message_id: true,
            ..AdapterCapabilities::default()
        };

        assert_eq!(
            command_target_fields(&message, true, &supported),
            ("event-1".into(), Some("activity-1".into()))
        );
        assert_eq!(
            command_target_fields(&message, false, &supported),
            ("activity-1".into(), None)
        );
        assert_eq!(
            command_target_fields(&message, true, &AdapterCapabilities::default()),
            ("activity-1".into(), None)
        );
    }

    #[test]
    fn client_hello_wire_shape_is_additive_and_versioned() {
        let value = serde_json::to_value(build_client_hello()).unwrap();
        assert_eq!(value["schema"], CLIENT_HELLO_SCHEMA);
        assert_eq!(value["protocol_version"], GATEWAY_PROTOCOL_VERSION);
        assert!(value["client_name"]
            .as_str()
            .is_some_and(|name| name.starts_with("openab-core/")));
        assert_eq!(value["requested_platforms"], serde_json::json!([]));
    }

    #[test]
    fn ws_path_filter_is_structural_only() {
        // The WS path's hoisted filter neuters channel/user checks (allow-all)
        // because L2/L3 moved to the shared trust registry (#1356 Phase 1c
        // prerequisite). This pins the two properties that combination relies
        // on: unknown channels/users PASS the structural filter (the gate
        // decides), while bot admission and @mention gating still apply.
        let no_ids: HashSet<String> = HashSet::new();
        let trusted: HashSet<String> = ["good-bot".to_string()].into_iter().collect();
        let filter = EventFilterParams {
            allow_all_channels: true,
            allowed_channels: &no_ids,
            allow_all_users: true,
            allowed_users: &no_ids,
            allow_bot_messages: false,
            trusted_bot_ids: &trusted,
            bot_username: Some("mybot"),
        };

        // Unknown human in unknown channel: structural filter passes it through.
        let ev = make_event(
            false,
            "stranger",
            "unlisted-channel",
            "private",
            None,
            vec!["mybot"],
        );
        assert!(!should_skip_event(&ev, &filter));

        // Untrusted bot still skipped (structural, stays on this path).
        let ev = make_event(
            true,
            "evil-bot",
            "unlisted-channel",
            "private",
            None,
            vec![],
        );
        assert!(should_skip_event(&ev, &filter));

        // Trusted bot admitted.
        let ev = make_event(
            true,
            "good-bot",
            "unlisted-channel",
            "private",
            None,
            vec![],
        );
        assert!(!should_skip_event(&ev, &filter));

        // Group without @mention still skipped (structural, stays on this path).
        let ev = make_event(false, "stranger", "group-1", "group", None, vec![]);
        assert!(should_skip_event(&ev, &filter));
    }

    #[test]
    fn echo_allowed_throttles_repeat_within_window() {
        // Unique key so we don't collide with other tests touching the global map.
        let key = "test-platform:test-sender-echo-throttle";
        assert!(echo_allowed(key), "first echo should be allowed");
        assert!(!echo_allowed(key), "immediate repeat should be throttled");
        assert!(!echo_allowed(key), "still throttled within the window");
    }

    fn make_event(is_bot: bool, sender_id: &str, channel_id: &str, channel_type: &str, thread_id: Option<&str>, mentions: Vec<&str>) -> GatewayEvent {
        serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.event.v1",
            "event_id": "evt1",
            "timestamp": "",
            "platform": "test",
            "channel": { "id": channel_id, "type": channel_type, "thread_id": thread_id },
            "sender": { "id": sender_id, "name": "user", "display_name": "User", "is_bot": is_bot },
            "content": { "type": "text", "text": "hello" },
            "mentions": mentions,
            "message_id": "msg1"
        })).unwrap()
    }

    fn make_teams_event(conversation_type: &str, is_dm: bool, mentions: Vec<&str>) -> GatewayEvent {
        let mut event = make_event(
            false,
            "29:user",
            "conversation-1",
            conversation_type,
            None,
            mentions,
        );
        event.platform = "teams".into();
        event.scope = Some(GwScope {
            tenant_id: Some("tenant-1".into()),
            team_id: Some("team-1".into()),
            channel_id: Some("channel-1".into()),
            conversation_type: conversation_type.into(),
            trust_scope_id: format!("teams:tenant-1:{conversation_type}:conversation-1"),
            is_dm,
        });
        event.recipient = Some(GwRecipient {
            id: "28:bot".into(),
            name: "OpenAB".into(),
        });
        event
    }

    fn teams_scope(event: &GatewayEvent) -> &GwScope {
        event.scope.as_ref().expect("Teams test event scope")
    }

    fn teams_router(allowed_users: Vec<String>) -> crate::adapter::AdapterRouter {
        let pool = Arc::new(crate::acp::SessionPool::new(
            crate::config::AgentConfig::default(),
            1,
            1,
            HashMap::new(),
        ));
        let mut trust = crate::trust::PlatformTrustConfigs::new();
        trust.insert(
            "teams",
            crate::trust::TrustConfig::new(
                Some(false),
                ["legacy-conversation".into()],
                Some(false),
                Some(false),
                allowed_users,
            ),
        );
        crate::adapter::AdapterRouter::new(
            pool,
            crate::config::ReactionsConfig::default(),
            crate::markdown::TableMode::Code,
            60,
            1,
            HashMap::new(),
            std::env::temp_dir(),
        )
        .with_trust(trust)
    }

    fn default_filter<'a>(allowed_channels: &'a HashSet<String>, allowed_users: &'a HashSet<String>, trusted_bot_ids: &'a HashSet<String>) -> EventFilterParams<'a> {
        EventFilterParams {
            allow_all_channels: true,
            allowed_channels,
            allow_all_users: true,
            allowed_users,
            allow_bot_messages: false,
            trusted_bot_ids,
            bot_username: None,
        }
    }

    #[test]
    fn bot_blocked_by_default() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let filter = default_filter(&ch, &us, &tb);
        let event = make_event(true, "bot1", "ch1", "dm", None, vec![]);
        assert!(should_skip_event(&event, &filter));
    }

    #[test]
    fn trusted_bot_passes() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb: HashSet<String> = ["bot1".into()].into();
        let filter = default_filter(&ch, &us, &tb);
        let event = make_event(true, "bot1", "ch1", "dm", None, vec![]);
        assert!(!should_skip_event(&event, &filter));
    }

    #[test]
    fn all_bots_allowed() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.allow_bot_messages = true;
        let event = make_event(true, "bot1", "ch1", "dm", None, vec![]);
        assert!(!should_skip_event(&event, &filter));
    }

    #[test]
    fn channel_allowlist_blocks() {
        let ch: HashSet<String> = ["allowed_ch".into()].into();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.allow_all_channels = false;
        let event = make_event(false, "u1", "other_ch", "dm", None, vec![]);
        assert!(should_skip_event(&event, &filter));
    }

    #[test]
    fn channel_allowlist_passes() {
        let ch: HashSet<String> = ["ch1".into()].into();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.allow_all_channels = false;
        let event = make_event(false, "u1", "ch1", "dm", None, vec![]);
        assert!(!should_skip_event(&event, &filter));
    }

    #[test]
    fn user_allowlist_blocks() {
        let ch = HashSet::new();
        let us: HashSet<String> = ["allowed_user".into()].into();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.allow_all_users = false;
        let event = make_event(false, "other_user", "ch1", "dm", None, vec![]);
        assert!(should_skip_event(&event, &filter));
    }

    #[test]
    fn group_without_mention_skipped() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.bot_username = Some("mybot");
        let event = make_event(false, "u1", "ch1", "group", None, vec![]);
        assert!(should_skip_event(&event, &filter));
    }

    #[test]
    fn group_with_mention_passes() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.bot_username = Some("mybot");
        let event = make_event(false, "u1", "ch1", "group", None, vec!["mybot"]);
        assert!(!should_skip_event(&event, &filter));
    }

    #[test]
    fn thread_in_group_bypasses_mention_gating() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let mut filter = default_filter(&ch, &us, &tb);
        filter.bot_username = Some("mybot");
        let event = make_event(false, "u1", "ch1", "group", Some("thread1"), vec![]);
        assert!(!should_skip_event(&event, &filter));
    }

    #[test]
    fn teams_trigger_matrix_uses_recipient_entity_ids() {
        let ch = HashSet::new();
        let us = HashSet::new();
        let tb = HashSet::new();
        let filter = default_filter(&ch, &us, &tb);

        let personal = make_teams_event("personal", true, vec![]);
        assert!(!should_skip_event(&personal, &filter));

        let mut unmentioned_group = make_teams_event("groupChat", false, vec![]);
        unmentioned_group.content.text = "@OpenAB <at>OpenAB</at> spoof".into();
        assert!(should_skip_event(&unmentioned_group, &filter));
        let mentioned_group = make_teams_event("groupChat", false, vec!["28:bot"]);
        assert!(!should_skip_event(&mentioned_group, &filter));
        let multi_mention = make_teams_event("groupChat", false, vec!["29:other", "28:bot"]);
        assert!(!should_skip_event(&multi_mention, &filter));
        let other_mention = make_teams_event("groupChat", false, vec!["29:other"]);
        assert!(should_skip_event(&other_mention, &filter));
        let mut missing_recipient = make_teams_event("groupChat", false, vec!["28:bot"]);
        missing_recipient.recipient = None;
        assert!(should_skip_event(&missing_recipient, &filter));

        let mut threaded_channel = make_teams_event("channel", false, vec![]);
        threaded_channel.channel.thread_id = Some("reply-chain".into());
        assert!(
            should_skip_event(&threaded_channel, &filter),
            "Teams thread presence must not bypass structured mention gating"
        );
        threaded_channel.mentions.push("28:bot".into());
        assert!(
            !should_skip_event(&threaded_channel, &filter),
            "a structured recipient mention must trigger in a channel reply"
        );

        let malformed_personal = make_teams_event("personal", false, vec![]);
        assert!(should_skip_event(&malformed_personal, &filter));
        let unknown = make_teams_event("meeting", false, vec!["28:bot"]);
        assert!(should_skip_event(&unknown, &filter));
    }

    #[test]
    fn teams_recipient_mention_cleanup_preserves_other_mentions() {
        let mut non_teams = make_event(false, "u1", "channel-1", "group", None, vec![]);
        non_teams.content.text = "  unchanged  ".into();
        assert_eq!(strip_recipient_mention(&non_teams), "  unchanged  ");

        let mut event = make_teams_event("channel", false, vec!["29:other", "28:bot"]);
        event.content.text = "<at>Same</at> ask <at>Same</at>  now".into();
        event.mention_entities = vec![
            GwMention {
                id: "29:other".into(),
                text: "<at>Same</at>".into(),
            },
            GwMention {
                id: "28:bot".into(),
                text: "<at>Same</at>".into(),
            },
        ];
        assert_eq!(strip_recipient_mention(&event), "<at>Same</at> ask   now");

        event.content.text = "<at>OpenAB</at> /reset".into();
        event.mention_entities = vec![GwMention {
            id: "28:bot".into(),
            text: "<at>OpenAB</at>".into(),
        }];
        assert_eq!(strip_recipient_mention(&event), "/reset");
        event.content.text = "  <at>OpenAB</at>  ".into();
        assert!(strip_recipient_mention(&event).is_empty());

        event.content.text = "<at>OpenAB</at> spoof".into();
        event.mention_entities.clear();
        assert_eq!(
            strip_recipient_mention(&event),
            "<at>OpenAB</at> spoof",
            "markup without an entity must remain ordinary text"
        );

        event.mention_entities.push(GwMention {
            id: "28:bot".into(),
            text: String::new(),
        });
        assert_eq!(strip_recipient_mention(&event), "<at>OpenAB</at> spoof");

        event.content.text = "<at>OpenAB</at> one <at>OpenAB</at> two".into();
        event.mention_entities = vec![
            GwMention {
                id: "28:bot".into(),
                text: "<at>OpenAB</at>".into(),
            },
            GwMention {
                id: "28:bot".into(),
                text: "<at>OpenAB</at>".into(),
            },
        ];
        assert_eq!(strip_recipient_mention(&event), "one  two");

        event.content.text = "text without matching markup".into();
        event.mention_entities = vec![GwMention {
            id: "28:bot".into(),
            text: "<at>OpenAB</at>".into(),
        }];
        assert_eq!(
            strip_recipient_mention(&event),
            "text without matching markup"
        );
    }

    #[test]
    fn teams_typed_scope_policy_is_kind_aware_and_legacy_compatible() {
        let typed = TeamsScopePolicy::new(
            true,
            ["team-1".into()],
            ["channel-2".into()],
            true,
            false,
            false,
            ["legacy-conversation".into()],
        );
        let personal = make_teams_event("personal", true, vec![]);
        assert!(typed.surface_allowed(
            &personal.channel.id,
            &personal.channel.channel_type,
            teams_scope(&personal)
        ));
        let group = make_teams_event("groupChat", false, vec!["28:bot"]);
        assert!(!typed.surface_allowed(
            &group.channel.id,
            &group.channel.channel_type,
            teams_scope(&group)
        ));
        let channel = make_teams_event("channel", false, vec!["28:bot"]);
        assert!(typed.surface_allowed(
            &channel.channel.id,
            &channel.channel.channel_type,
            teams_scope(&channel)
        ));

        let mut channel_match = channel.clone();
        let scope = channel_match
            .scope
            .as_mut()
            .expect("Teams test event scope");
        scope.team_id = Some("other-team".into());
        scope.channel_id = Some("channel-2".into());
        assert!(typed.surface_allowed(
            &channel_match.channel.id,
            &channel_match.channel.channel_type,
            scope
        ));
        scope.channel_id = None;
        assert!(!typed.surface_allowed(
            &channel_match.channel.id,
            &channel_match.channel.channel_type,
            scope
        ));

        let typed_open = TeamsScopePolicy::new(
            true,
            Vec::<String>::new(),
            Vec::<String>::new(),
            false,
            true,
            false,
            Vec::<String>::new(),
        );
        assert!(typed_open.surface_allowed(
            &channel.channel.id,
            &channel.channel.channel_type,
            teams_scope(&channel)
        ));
        assert!(!typed_open.surface_allowed(
            &personal.channel.id,
            &personal.channel.channel_type,
            teams_scope(&personal)
        ));

        let legacy = TeamsScopePolicy::new(
            false,
            Vec::<String>::new(),
            Vec::<String>::new(),
            true,
            true,
            false,
            ["conversation-1".into()],
        );
        assert!(legacy.surface_allowed(
            &channel.channel.id,
            &channel.channel.channel_type,
            teams_scope(&channel)
        ));
        assert!(!legacy.surface_allowed(
            "other-conversation",
            &channel.channel.channel_type,
            teams_scope(&channel)
        ));
    }

    #[test]
    fn teams_scope_shape_validation_fails_closed() {
        let personal = make_teams_event("personal", true, vec![]);
        assert!(typed_scope_shape_is_valid(
            &personal.channel.id,
            &personal.channel.channel_type,
            teams_scope(&personal)
        ));

        let mut malformed = personal.clone();
        malformed.scope.as_mut().expect("scope").tenant_id = None;
        assert!(!typed_scope_shape_is_valid(
            &malformed.channel.id,
            &malformed.channel.channel_type,
            teams_scope(&malformed)
        ));

        let mut malformed = personal.clone();
        malformed.scope.as_mut().expect("scope").trust_scope_id = "  ".into();
        assert!(!typed_scope_shape_is_valid(
            &malformed.channel.id,
            &malformed.channel.channel_type,
            teams_scope(&malformed)
        ));

        let mut malformed = personal.clone();
        malformed.scope.as_mut().expect("scope").is_dm = false;
        assert!(!typed_scope_shape_is_valid(
            &malformed.channel.id,
            &malformed.channel.channel_type,
            teams_scope(&malformed)
        ));

        let mut malformed = personal.clone();
        malformed.channel.id.clear();
        assert!(!typed_scope_shape_is_valid(
            &malformed.channel.id,
            &malformed.channel.channel_type,
            teams_scope(&malformed)
        ));

        let channel = make_teams_event("channel", false, vec!["28:bot"]);
        assert!(typed_scope_shape_is_valid(
            &channel.channel.id,
            &channel.channel.channel_type,
            teams_scope(&channel)
        ));
        let mut missing_team = channel.clone();
        missing_team.scope.as_mut().expect("scope").team_id = None;
        assert!(!typed_scope_shape_is_valid(
            &missing_team.channel.id,
            &missing_team.channel.channel_type,
            teams_scope(&missing_team)
        ));
        let mut missing_channel = channel.clone();
        missing_channel.scope.as_mut().expect("scope").channel_id = None;
        assert!(!typed_scope_shape_is_valid(
            &missing_channel.channel.id,
            &missing_channel.channel.channel_type,
            teams_scope(&missing_channel)
        ));
        let mut mismatched_type = channel.clone();
        mismatched_type
            .scope
            .as_mut()
            .expect("scope")
            .conversation_type = "groupChat".into();
        assert!(!typed_scope_shape_is_valid(
            &mismatched_type.channel.id,
            &mismatched_type.channel.channel_type,
            teams_scope(&mismatched_type)
        ));

        let unknown = make_teams_event("meeting", false, vec!["28:bot"]);
        assert!(!typed_scope_shape_is_valid(
            &unknown.channel.id,
            &unknown.channel.channel_type,
            teams_scope(&unknown)
        ));
    }

    #[test]
    fn teams_gate_orders_typed_scope_before_l3_and_keeps_legacy_fallback() {
        let router = teams_router(vec!["29:user".into()]);
        let typed = TeamsScopePolicy::new(
            true,
            ["team-1".into()],
            Vec::<String>::new(),
            true,
            true,
            false,
            ["legacy-conversation".into()],
        );
        let channel = make_teams_event("channel", false, vec!["28:bot"]);
        assert!(matches!(
            gate_gateway_event(&router, &channel, &typed),
            GateOutcome::Allow
        ));

        let mut untrusted = channel.clone();
        untrusted.sender.id = "29:untrusted".into();
        assert!(matches!(
            gate_gateway_event(&router, &untrusted, &typed),
            GateOutcome::Deny { echo: Some(_) }
        ));

        let mut malformed = untrusted;
        malformed.scope.as_mut().expect("scope").team_id = None;
        assert!(matches!(
            gate_gateway_event(&router, &malformed, &typed),
            GateOutcome::Deny { echo: None }
        ));

        let legacy = TeamsScopePolicy::new(
            false,
            Vec::<String>::new(),
            Vec::<String>::new(),
            true,
            true,
            false,
            ["legacy-conversation".into()],
        );
        let mut old_event = make_teams_event("channel", false, vec![]);
        old_event.scope = None;
        old_event.channel.id = "legacy-conversation".into();
        assert!(matches!(
            gate_gateway_event(&router, &old_event, &legacy),
            GateOutcome::Allow
        ));
        old_event.channel.id = "other-conversation".into();
        assert!(matches!(
            gate_gateway_event(&router, &old_event, &legacy),
            GateOutcome::Deny { echo: None }
        ));
    }

    #[test]
    fn gateway_event_typed_teams_fields_decode_additively() {
        let legacy = make_event(false, "u1", "conversation-1", "groupChat", None, vec![]);
        assert!(legacy.scope.is_none());
        assert!(legacy.recipient.is_none());
        assert!(legacy.mention_entities.is_empty());

        let modern: GatewayEvent = serde_json::from_value(serde_json::json!({
            "schema": "openab.gateway.event.v1",
            "event_id": "evt1",
            "timestamp": "2024-01-01T00:00:00Z",
            "platform": "teams",
            "bot_id": "28:bot",
            "sender": {
                "id": "29:user",
                "name": "user",
                "display_name": "User",
                "is_bot": false
            },
            "channel": { "id": "conversation-1", "type": "channel" },
            "content": {
                "type": "text",
                "text": "<at>OpenAB</at> hello",
                "attachments": [{
                    "type": "image",
                    "filename": "image.png",
                    "mime_type": "image/png",
                    "reference": "att-opaque",
                    "size": 0
                }]
            },
            "mentions": ["28:bot"],
            "message_id": "msg1",
            "scope": {
                "tenant_id": "tenant-1",
                "team_id": "team-1",
                "channel_id": "channel-1",
                "conversation_type": "channel",
                "trust_scope_id": "teams:tenant-1:team:team-1:channel:channel-1",
                "is_dm": false
            },
            "recipient": { "id": "28:bot", "name": "OpenAB" },
            "mention_entities": [
                { "id": "28:bot", "text": "<at>OpenAB</at>" }
            ]
        }))
        .expect("typed Teams Gateway event should decode");

        assert_eq!(teams_scope(&modern).team_id.as_deref(), Some("team-1"));
        assert_eq!(
            modern.recipient.as_ref().map(|r| r.id.as_str()),
            Some("28:bot")
        );
        assert_eq!(modern.mention_entities.len(), 1);
    }
}

/// Render a channel id for logs, hashing it when it is an ACP channel or session id.
///
/// An ACP `channel_id` is `acp_<uuid>` and the session id is `sess_<same uuid>`, so the two are
/// mutually derivable: either form printed in full IS a resume credential. Anyone reading operator
/// logs could resume the session, and logs travel further than the sessions they describe.
///
/// **The uuid is hashed, not the prefixed string.** One session reaches this function as
/// `acp_<uuid>` and elsewhere as `sess_<uuid>`; hashing the whole string gives those two forms a
/// different tag each, and a third different again from [`crate::redact::redact_session_ids`] and
/// the gateway's `redact_id`, which strip the prefix first. Several tags for one session defeat the
/// only purpose the tag has — following that session across logs — more completely than not
/// redacting would, and it has already read as zero overlap between two logs describing the same
/// session.
///
/// Only ACP ids are hashed. A Discord or Slack channel id is a public identifier that operators
/// legitimately grep for, and redacting it would cost real debuggability to protect nothing.
///
/// Copies of this function live in `openab-gateway` and `openab-mcp` because those crates
/// deliberately do not depend on this one. Each is pinned to the same vector; where a crate has a
/// redactor of its own, its test compares against that rather than against a copied literal.
fn redact_channel(id: &str) -> String {
    let Some(uuid) = id
        .strip_prefix("acp_")
        .or_else(|| id.strip_prefix("sess_"))
        .filter(|uuid| !uuid.is_empty())
    else {
        return id.to_string();
    };
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(uuid.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("#{short}")
}

#[cfg(test)]
mod redact_channel_tests {
    const CHANNEL: &str = "acp_00000000-0000-0000-0000-000000000000";
    const SESSION: &str = "sess_00000000-0000-0000-0000-000000000000";

    /// The tag for a given session must be IDENTICAL in every crate that logs a channel id, and
    /// identical across the two forms one session is addressed by.
    ///
    /// `#12b9377c` is the uuid's tag, shared with `redact_session_ids` and the gateway's
    /// `redact_id`. It used to be `#850414fa` here, the hash of the whole `acp_<uuid>` string, which
    /// is why one session could appear under two tags depending on which log you were reading.
    ///
    /// The literal is pinned in all three crates, but this crate has its own redactor, so the
    /// assertion that matters is the comparison against it: a divergence then fails without anyone
    /// having to remember to update three copied literals.
    #[test]
    fn an_acp_id_hashes_its_uuid_to_the_shared_vector_and_others_pass_through() {
        assert_eq!(
            super::redact_channel(CHANNEL),
            "#12b9377c",
            "ACP channel ids must hash to the tag the other crates produce for the same session"
        );
        assert_eq!(
            super::redact_channel(SESSION),
            "#12b9377c",
            "both forms of one session must share a tag — hashing the prefix is what split them"
        );
        assert_eq!(
            super::redact_channel(CHANNEL),
            crate::redact::redact_session_ids(CHANNEL),
            "this crate's two redactors must agree without relying on a copied literal"
        );
        assert_eq!(
            super::redact_channel(SESSION),
            crate::redact::redact_session_ids(SESSION),
            "including on the session form, which used to pass through here unredacted"
        );
        assert_eq!(
            super::redact_channel("1234567890"),
            "1234567890",
            "a non-ACP channel id is a public identifier and must stay greppable"
        );
        assert_eq!(
            super::redact_channel("-"),
            "-",
            "the no-session sentinel must not be hashed into something that looks like a session"
        );
    }
}

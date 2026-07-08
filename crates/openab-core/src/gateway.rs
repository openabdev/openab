use crate::acp::ContentBlock;
use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef, SenderContext};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

/// Timeout for waiting on gateway reply acknowledgement.
const GATEWAY_REPLY_TIMEOUT_SECS: u64 = 5;

/// Platforms whose gateway adapter emits a `GatewayResponse` for `edit_message`
/// so core can observe edit success or failure (used to gate the per-edit
/// response-wait below).
///
/// Today only Feishu does, because it is the only adapter with a known
/// per-message edit cap (errcode 230072) that requires core-side recovery, and
/// the only one wired to ack edits.
///
/// NOTE: this gates the `edit_message` response-wait only. `delete_message` is
/// unconditionally fire-and-forget (the recovery path sends fresh content
/// regardless of the delete outcome), so it does not consult this list.
///
/// TECH DEBT: this is platform-identity standing in for a *capability*. The
/// right model is a capability handshake at gateway-connect time ("does this
/// adapter acknowledge edits?") rather than a hardcoded platform name. We
/// accept the hardcode now because there is no handshake protocol yet; when one
/// lands, replace this allowlist with a negotiated capability flag. Any new
/// adapter that wires request/response for edits MUST be added here, or its
/// edit failures stay invisible to core (silent failure mode).
const EDIT_RESPONSE_PLATFORMS: &[&str] = &["feishu"];

/// Whether `platform` acknowledges `edit_message` with a `GatewayResponse`.
/// See `EDIT_RESPONSE_PLATFORMS`.
fn platform_acks_writes(platform: &str) -> bool {
    EDIT_RESPONSE_PLATFORMS.contains(&platform)
}

/// Gateway platforms whose messaging API cannot edit a message after it is sent.
///
/// Cosmetic (typewriter) streaming works by posting a placeholder and then
/// repeatedly editing it in place with the growing text. On a platform with no
/// edit endpoint, each of those "edits" is delivered as a brand-new message
/// instead — so the user sees the same reply posted several times, each copy
/// longer than the last. Streaming is therefore force-disabled (send-once) for
/// these platforms regardless of the configured `streaming` flag.
///
/// LINE's Messaging API only exposes reply/push (no edit), so it lives here.
/// (The in-process unified adapter additionally hard-drops stray edit_message
/// commands in the LINE adapter itself — see `dispatch_line_reply`.)
///
/// NOTE: like `EDIT_RESPONSE_PLATFORMS`, this is platform-identity standing in
/// for a *capability*. The right long-term model is a capability handshake at
/// gateway-connect time ("can this adapter edit messages?"); until that exists,
/// any new gateway platform that lacks a message-edit API MUST be added here.
const NON_EDITABLE_PLATFORMS: &[&str] = &["line"];

/// Whether cosmetic streaming (placeholder + in-place edits) is possible on
/// `platform`. See `NON_EDITABLE_PLATFORMS`.
fn platform_supports_streaming(platform: &str) -> bool {
    !NON_EDITABLE_PLATFORMS.contains(&platform)
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
        tracing::info!(channel = %event.channel.id, "gateway: channel not in allowed_channels, skipping");
        return true;
    }
    // User allowlist
    if !filter.allow_all_users && !filter.allowed_users.contains(&event.sender.id) {
        tracing::info!(sender = %event.sender.id, "gateway: user not in allowed_users, skipping");
        return true;
    }
    // @mention gating: in groups, only respond if bot is mentioned
    let is_group = event.channel.channel_type == "group" || event.channel.channel_type == "supergroup";
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
    #[allow(dead_code)]
    mentions: Vec<String>,
    message_id: String,
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
}

// --- GatewayAdapter: ChatAdapter over WebSocket ---

type PendingRequests = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<GatewayResponse>>>>;
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

pub struct GatewayAdapter {
    ws_tx: SharedWsTx,
    pending: PendingRequests,
    platform_name: &'static str,
    streaming: bool,
    streaming_placeholder: bool,
    telegram_rich_messages: bool,
}

impl GatewayAdapter {
    fn new(
        ws_tx: SharedWsTx,
        pending: PendingRequests,
        platform_name: &'static str,
        streaming: bool,
        streaming_placeholder: bool,
        telegram_rich_messages: bool,
    ) -> Self {
        Self {
            ws_tx,
            pending,
            platform_name,
            streaming,
            streaming_placeholder,
            telegram_rich_messages,
        }
    }

    /// Internal helper for send_message / send_message_with_reply.
    async fn send_gateway_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        quote_message_id: Option<&str>,
    ) -> Result<MessageRef> {
        let req_id = if self.streaming {
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
                text: content.into(),
            },
            command: None,
            request_id: req_id.clone(),
            quote_message_id: quote_message_id.map(|s| s.to_string()),
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(e) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            if let Some(ref id) = req_id {
                self.pending.lock().await.remove(id);
            }
            return Err(e.into());
        }
        let msg_id = if let (Some(rx), Some(ref id)) = (pending_rx, &req_id) {
            match tokio::time::timeout(std::time::Duration::from_secs(GATEWAY_REPLY_TIMEOUT_SECS), rx).await {
                Ok(Ok(resp)) if resp.success => resp.message_id.unwrap_or_else(|| "gw_sent".into()),
                Ok(Ok(resp)) => {
                    // Gateway explicitly reported failure (success=false). Surface
                    // as Err so dispatch sets ❌ instead of 🆗 over an incomplete
                    // delivery. Examples: Feishu edit cap reached after append-new
                    // fallback also failed; chunked send delivered N/M chunks.
                    let err_msg = resp.error.clone()
                        .unwrap_or_else(|| "gateway reported failure".to_string());
                    tracing::warn!(request_id = %id, error = %err_msg, "gateway replied with failure");
                    return Err(anyhow::anyhow!("gateway reported failure: {err_msg}"));
                }
                Ok(Err(_)) => {
                    // Channel closed (gateway shutting down or pending dropped).
                    // Maintain legacy behavior — adapters that don't implement
                    // GatewayResponse for all reply types (LINE, Teams) rely on
                    // this for non-failure outcomes.
                    tracing::warn!(request_id = %id, "gateway response channel closed");
                    "gw_sent".into()
                }
                Err(_) => {
                    // Timeout. Many adapters (LINE, Teams) intentionally do not
                    // emit GatewayResponse for replies, so timeout is the expected
                    // path for them. Maintain legacy behavior to avoid breaking
                    // platforms that have not yet wired request/response feedback.
                    tracing::warn!(request_id = %id, "gateway reply timed out");
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
}

/// Send a fire-and-forget reply via the shared WebSocket (no request-response).
/// Used for slash command responses where we don't need message_id back.
async fn send_fire_and_forget(
    ws_tx: &SharedWsTx,
    channel: &ChannelRef,
    content: &str,
) -> Result<()> {
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
            text: content.into(),
        },
        command: None,
        request_id: None,
        quote_message_id: None,
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
        4096 // Telegram limit
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
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
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
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
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
        // Scope intentionally limited to platforms that ack writes (see
        // EDIT_RESPONSE_PLATFORMS). Other adapters (LINE, Teams, Slack, Discord,
        // …) keep the original fire-and-forget path so cosmetic streaming on
        // those platforms does not pay a response-wait penalty per flush.
        const EDIT_RESPONSE_TIMEOUT_MS: u64 = 800;
        let needs_response = self.streaming && platform_acks_writes(&msg.channel.platform);

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
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
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
            request_id: req_id.clone(),
        };
        let json = serde_json::to_string(&reply)?;
        if let Err(e) = self.ws_tx.lock().await.send(Message::Text(json)).await {
            if let Some(ref id) = req_id {
                self.pending.lock().await.remove(id);
            }
            return Err(e.into());
        }
        if let (Some(rx), Some(ref id)) = (pending_rx, &req_id) {
            match tokio::time::timeout(
                std::time::Duration::from_millis(EDIT_RESPONSE_TIMEOUT_MS),
                rx,
            ).await {
                Ok(Ok(resp)) if resp.success => Ok(()),
                Ok(Ok(resp)) => {
                    let err_msg = resp.error.clone()
                        .unwrap_or_else(|| "gateway reported edit failure".to_string());
                    tracing::warn!(request_id = %id, error = %err_msg, "edit_message gateway replied failure");
                    Err(anyhow::anyhow!("edit failure: {err_msg}"))
                }
                Ok(Err(_)) => {
                    tracing::debug!(request_id = %id, "edit_message gateway response channel closed");
                    Ok(())
                }
                Err(_) => {
                    // Timeout — feishu didn't respond within the window
                    // (probably a slow API). Treat as success to avoid
                    // false-positive ❌; the cap-reached path already short-
                    // circuits much faster (gateway returns immediately).
                    self.pending.lock().await.remove(id);
                    Ok(())
                }
            }
        } else {
            // Non-feishu (or non-streaming): fire-and-forget, no added latency.
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
    /// Fire-and-forget: gateway adapters that don't implement delete will simply
    /// ignore the command. Failure is non-fatal — if delete fails, the user sees
    /// the placeholder remain (same behavior as before this override). We do not
    /// wait on a response here: the recovery path sends fresh content regardless
    /// of whether the delete landed, so a response would only buy an extra log
    /// line at the cost of a per-finalize wait.
    async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
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
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        self.streaming
    }

    fn show_streaming_placeholder(&self) -> bool {
        self.streaming_placeholder
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
pub struct GatewayParams {
    pub url: String,
    pub platform: String,
    pub token: Option<String>,
    pub bot_username: Option<String>,
    pub allow_all_channels: bool,
    pub allowed_channels: Vec<String>,
    pub allow_all_users: bool,
    pub allowed_users: Vec<String>,
    pub allow_bot_messages: bool,
    pub trusted_bot_ids: Vec<String>,
    pub streaming: bool,
    pub streaming_placeholder: bool,
    pub telegram_rich_messages: bool,
    pub stt: crate::config::SttConfig,
}

pub async fn run_gateway_adapter(
    params: GatewayParams,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    dispatcher: Arc<crate::dispatch::Dispatcher>,
    router: Arc<crate::adapter::AdapterRouter>,
) -> Result<()> {
    let platform: &'static str = Box::leak(params.platform.into_boxed_str());

    // Append auth token as query param if configured
    let gateway_url = params.url;
    let bot_username = params.bot_username;
    let allow_all_channels = params.allow_all_channels;
    let allowed_channels: HashSet<String> = params.allowed_channels.into_iter().collect();
    let allow_all_users = params.allow_all_users;
    let allowed_users: HashSet<String> = params.allowed_users.into_iter().collect();
    let allow_bot_messages = params.allow_bot_messages;
    let trusted_bot_ids: HashSet<String> = params.trusted_bot_ids.into_iter().collect();
    // Cosmetic streaming edits a placeholder in place. On platforms without an
    // edit API (e.g. LINE) every edit lands as a new message — growing
    // duplicates — so force send-once mode there regardless of config.
    let streaming = if params.streaming && !platform_supports_streaming(platform) {
        warn!(
            platform,
            "streaming is enabled but this platform cannot edit messages; \
             forcing send-once mode to avoid duplicate messages"
        );
        false
    } else {
        params.streaming
    };
    let streaming_placeholder = params.streaming_placeholder;
    let telegram_rich_messages = params.telegram_rich_messages;
    let stt_config = params.stt;

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

        let ws_stream = match tokio_tungstenite::connect_async(&connect_url).await {
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
        let adapter: Arc<dyn ChatAdapter> = Arc::new(GatewayAdapter::new(
            ws_tx.clone(),
            pending.clone(),
            platform,
            streaming,
            streaming_placeholder,
            telegram_rich_messages,
        ));
        let slash_ws_tx = ws_tx.clone(); // for fire-and-forget slash command responses
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        // Hoist filter params outside loop — all fields are loop-invariant
        let filter = EventFilterParams {
            allow_all_channels,
            allowed_channels: &allowed_channels,
            allow_all_users,
            allowed_users: &allowed_users,
            allow_bot_messages,
            trusted_bot_ids: &trusted_bot_ids,
            bot_username: bot_username.as_deref(),
        };

        loop {
            tokio::select! {
                    msg = ws_rx.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                let text_str: &str = &text;

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
                                    if should_skip_event(&event, &filter) {
                                        continue;
                                    }

                                    info!(
                                        platform = %event.platform,
                                        sender = %event.sender.name,
                                        channel = %event.channel.id,
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
                                        receiver_id: None, // gateway does not yet resolve receiver identity
                                    };
                                    let sender_json = serde_json::to_string(&sender_ctx)
                                        .unwrap_or_default();

                                    let trigger_msg = MessageRef {
                                        channel: channel.clone(),
                                        message_id: event.message_id.clone(),
                                    };

                                    let adapter = adapter.clone();
                                    let prompt = event.content.text.clone();
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
                                                    let text = String::from_utf8_lossy(&bytes);
                                                    extra_blocks.push(ContentBlock::Text {
                                                        text: format!("```{}\n{}\n```", att.filename, text),
                                                    });
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
                        info!("gateway adapter shutting down, waiting for {} in-flight tasks", tasks.len());
                        while tasks.join_next().await.is_some() {}
                        return Ok(());
                    }
                }
            }
        } // inner loop — break here means reconnect

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
pub struct GatewayEventContext {
    pub adapter: Arc<dyn ChatAdapter>,
    pub dispatcher: Arc<crate::dispatch::Dispatcher>,
    pub router: Arc<crate::adapter::AdapterRouter>,
    pub allow_all_channels: bool,
    pub allowed_channels: HashSet<String>,
    pub allow_all_users: bool,
    pub allowed_users: HashSet<String>,
    pub allow_bot_messages: bool,
    pub trusted_bot_ids: HashSet<String>,
    pub bot_username: Option<String>,
    pub stt_config: crate::config::SttConfig,
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
    // Phase 1: `is_dm = false` preserves today's behavior where gateway DMs are
    // evaluated against the channel allowlist like any other channel (the
    // `allow_dm` surface semantics arrive with the per-platform trust flip).
    // TODO(phase-2): derive is_dm from the event/ChannelRef carrier so the
    // `allow_dm` L2 surface can be enforced and tested for gateway platforms.
    let decision =
        ctx.router
            .gate_incoming(&event.platform, &event.channel.id, false, &event.sender.id);
    match decision {
        crate::trust::Decision::Allow => {}
        crate::trust::Decision::DenyIdentity => {
            // L3 identity deny → echo the sender their ID so they can request
            // access (throttled to avoid amplification). Bots never reach here
            // (should_skip_event handles bot admission; L3 is human-only).
            tracing::info!(
                platform = %event.platform,
                sender = %event.sender.id,
                channel = %event.channel.id,
                "gateway event denied (identity); echoing request-access"
            );
            let throttle_key = format!("{}:{}", event.platform, event.sender.id);
            if echo_allowed(&throttle_key) {
                let echo_channel = ChannelRef {
                    platform: event.platform.clone(),
                    channel_id: event.channel.id.clone(),
                    thread_id: event.channel.thread_id.clone(),
                    parent_id: None,
                    origin_event_id: Some(event.event_id.clone()),
                };
                let echo = format!(
                    "⚠️ You are not on this bot's trusted list.\nYour ID: {}\nAsk the admin to add it to allowed_users.",
                    event.sender.id
                );
                let _ = ctx.adapter.send_message(&echo_channel, &echo).await;
            }
            return Ok(false);
        }
        // DenyScope (and any future variant) → silent drop (scope is not a
        // security boundary; no request-access echo).
        _ => {
            tracing::info!(
                platform = %event.platform,
                sender = %event.sender.id,
                channel = %event.channel.id,
                ?decision,
                "gateway event denied (scope); silent"
            );
            return Ok(false);
        }
    }

    tracing::info!(
        platform = %event.platform,
        sender = %event.sender.name,
        channel = %event.channel.id,
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
        receiver_id: None,
    };
    let sender_json = serde_json::to_string(&sender_ctx).unwrap_or_default();

    let trigger_msg = MessageRef {
        channel: channel.clone(),
        message_id: event.message_id.clone(),
    };

    // Convert gateway attachments to ContentBlocks
    let mut extra_blocks = Vec::new();
    for att in &event.content.attachments {
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
                match bytes_result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        extra_blocks.push(ContentBlock::Text {
                            text: format!("```{}\n{}\n```", att.filename, text),
                        });
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

    // Slash command interception
    let prompt = event.content.text.clone();
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
    use std::collections::HashSet;

    #[test]
    fn line_cannot_stream_and_is_forced_send_once() {
        // LINE has no message-edit API, so cosmetic streaming is impossible.
        assert!(!platform_supports_streaming("line"));
    }

    #[test]
    fn editable_platforms_still_allow_streaming() {
        for platform in [
            "telegram",
            "slack",
            "discord",
            "feishu",
            "teams",
            "googlechat",
            "wecom",
        ] {
            assert!(
                platform_supports_streaming(platform),
                "{platform} should still support streaming",
            );
        }
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
}

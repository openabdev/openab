use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use tracing::{error, warn};

use crate::acp::{classify_notification, parse_turn_result, AcpEvent, ContentBlock, SessionPool, TurnResult};
use crate::config::{ReactionsConfig, ToolDisplay};
use crate::error_display::{format_coded_error, format_user_error};
use crate::format;
use crate::markdown::{self, TableMode};
use crate::reactions::StatusReactionController;

// --- Output directive parsing ---

/// Parsed directives from agent output header block.
/// Consecutive `[[key:value]]` lines at the start of output are directives.
#[derive(Default, Debug)]
pub struct OutputDirectives {
    /// Message ID to reply to (Discord: message_reference)
    pub reply_to: Option<String>,
}

/// Parse `[[key:value]]` directives from the beginning of agent output.
/// Returns parsed directives and the remaining content (directives stripped).
pub fn parse_output_directives(content: &str) -> (OutputDirectives, String) {
    let mut directives = OutputDirectives::default();
    let mut content_start = 0;
    let mut trailing_content: Option<&str> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        // Try to match [[key:value]] at the start of the line (lenient: allows trailing content)
        if let Some(after_open) = trimmed.strip_prefix("[[") {
            if let Some(close_pos) = after_open.find("]]") {
                let inner = &after_open[..close_pos];
                if let Some((key, value)) = inner.split_once(':') {
                    match key.trim() {
                        "reply_to" => {
                            let v = value.trim();
                            // Validate: non-empty, reasonable length, no whitespace/control chars
                            if !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_') {
                                directives.reply_to = Some(v.to_string());
                            }
                        }
                        _ => {
                            tracing::debug!(key = key.trim(), "unknown output directive ignored");
                        }
                    }
                    // Check for trailing content after ]]
                    let remainder = after_open[close_pos + 2..].trim();
                    if !remainder.is_empty() {
                        trailing_content = Some(remainder);
                        // Advance past this line
                        content_start += line.len();
                        if content.as_bytes().get(content_start) == Some(&b'\r') {
                            content_start += 1;
                        }
                        if content.as_bytes().get(content_start) == Some(&b'\n') {
                            content_start += 1;
                        }
                        break; // Trailing content ends directive header
                    }
                    // Advance past this line + its line ending (handles both \n and \r\n)
                    content_start += line.len();
                    if content.as_bytes().get(content_start) == Some(&b'\r') {
                        content_start += 1;
                    }
                    if content.as_bytes().get(content_start) == Some(&b'\n') {
                        content_start += 1;
                    }
                } else {
                    // [[X]] without colon — not a directive, stop parsing
                    break;
                }
            } else {
                // No closing ]] found — not a directive, stop parsing
                break;
            }
        } else {
            break;
        }
    }

    let remaining = if let Some(trailing) = trailing_content {
        if content_start < content.len() {
            format!("{}\n{}", trailing, &content[content_start..])
        } else {
            trailing.to_string()
        }
    } else if content_start < content.len() {
        content[content_start..].to_string()
    } else {
        String::new()
    };
    (directives, remaining)
}

/// Select the answer text to deliver from the turn's accumulated agent-message
/// buffer.
///
/// `full` is every `agent_message_chunk` concatenated across the turn, which
/// includes the inter-tool narration the agent emits between tool calls ("let
/// me pull the diff", "now reading the validator", ...). `answer_start` is the
/// byte offset where the final answer block begins — set to the buffer length
/// each time a tool call completes, so it ends up pointing just past the last
/// tool.
///
/// When `keep_full` is false we deliver only that final block, dropping the
/// narration so the message reads like the single composed artefact a
/// tool-posted comment is. `keep_full` is true when the reply was streamed
/// (the text was already shown live) or when `[reactions] narration_display` is
/// set; in that case the whole buffer is returned unchanged.
///
/// `answer_start` is always a previous `full.len()`, hence a valid char
/// boundary; the `get(..)` fallback to `full` only guards against a future
/// caller passing a stale offset.
pub fn select_delivery_text(full: &str, answer_start: usize, keep_full: bool) -> &str {
    if keep_full {
        full
    } else {
        full.get(answer_start..).unwrap_or_else(|| {
            tracing::warn!(
                answer_start,
                full_len = full.len(),
                "stale answer_start offset; delivering full buffer"
            );
            full
        })
    }
}

/// Resolve the directives and body to deliver for a finished turn.
///
/// Output directives (e.g. `[[reply_to:...]]`) sit at the very start of the
/// turn's output per `docs/output-directives.md`. When `keep_full` is false
/// (send-once trimming) that start can be inter-tool narration that
/// [`select_delivery_text`] discards — so we parse directives from the **full**
/// buffer (preserving them) and then take the body from the delivered slice.
///
/// The delivered slice is re-parsed to strip the directive header only when it
/// still starts at byte 0 (`answer_start == 0` or `keep_full`). When
/// `answer_start > 0` the slice is mid-buffer text; any `[[…]]` there is reply
/// content, not a directive header, and must not be stripped.
///
/// Note: directive preservation assumes the turn buffer starts with the
/// directive. A session-reset turn seeds the buffer with the expiry notice
/// first, so directives are not preserved in that case (pre-existing behaviour).
pub fn split_delivery(
    full: &str,
    answer_start: usize,
    keep_full: bool,
) -> (OutputDirectives, String) {
    let (directives, _) = parse_output_directives(full);
    let delivered = select_delivery_text(full, answer_start, keep_full);
    // Strip the directive header from the body only when the delivered slice
    // begins at byte 0 (no tools ran, or keep_full). When answer_start > 0,
    // delivered is the post-last-tool suffix — don't re-parse it.
    let body = if answer_start == 0 || keep_full {
        parse_output_directives(delivered).1
    } else {
        delivered.to_owned()
    };
    (directives, body)
}

/// Apply the session-reset re-prepend rule to a finalized turn body.
///
/// The session-reset notice (`"⚠️ _Session expired, starting fresh..._\n\n"`)
/// is pushed at the head of the turn buffer so streaming consumers see it
/// live. When the turn ends in send-once trimming mode (`!keep_full_text`) and
/// a tool ran (`answer_start > 0`), the slice that `select_delivery_text`
/// returns starts *after* the notice — so re-prepend it to keep the user
/// aware their session was reset. In every other corner (no reset, or
/// `keep_full_text`, or no tools ran) the notice is either absent or already
/// included in `body`, and we must not duplicate it.
///
/// Pure helper: deliberately mirrors the inline branch at the end of
/// `AdapterRouter::stream_prompt_blocks` so the four-corner truth table can be
/// exercised in isolation without a live ACP session.
pub(crate) fn finalize_body(
    reset: bool,
    keep_full_text: bool,
    answer_start: usize,
    body: String,
) -> String {
    if reset && !keep_full_text && answer_start > 0 {
        format!("⚠️ _Session expired, starting fresh..._\n\n{body}")
    } else {
        body
    }
}

// --- Platform-agnostic types ---

/// Identifies a channel or thread across platforms.
///
/// Used for **routing**: `channel_id` is the ID the adapter sends messages to.
/// For Discord threads, this is the thread's own channel ID (Discord API
/// requires it for `say`/`edit`). Use `parent_id` to find the parent channel.
///
/// Compare with `SenderContext`, which is **metadata for the agent**: there
/// `channel_id` is the parent channel and `thread_id` is the thread,
/// matching Slack's model for cross-platform consistency.
#[derive(Clone, Debug)]
pub struct ChannelRef {
    pub platform: String,
    pub channel_id: String,
    /// Thread within a channel (e.g. Slack thread_ts, Telegram topic_id).
    /// For Discord, threads are separate channels so this is None.
    pub thread_id: Option<String>,
    /// Parent channel if this is a thread-as-channel (Discord).
    pub parent_id: Option<String>,
    /// Originating gateway event ID, propagated back in `GatewayReply.reply_to`
    /// so the gateway can correlate replies with inbound events (e.g. LINE reply tokens).
    /// Excluded from Hash/Eq — two ChannelRefs pointing to the same channel are
    /// equal regardless of which event they originated from.
    pub origin_event_id: Option<String>,
}

impl PartialEq for ChannelRef {
    fn eq(&self, other: &Self) -> bool {
        self.platform == other.platform
            && self.channel_id == other.channel_id
            && self.thread_id == other.thread_id
            && self.parent_id == other.parent_id
    }
}

impl Eq for ChannelRef {}

impl std::hash::Hash for ChannelRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.platform.hash(state);
        self.channel_id.hash(state);
        self.thread_id.hash(state);
        self.parent_id.hash(state);
    }
}

/// Identifies a message across platforms.
#[derive(Clone, Debug)]
pub struct MessageRef {
    pub channel: ChannelRef,
    pub message_id: String,
}

/// Bundles per-message parameters for `AdapterRouter::handle_message`.
///
/// Introduced to reduce parameter count and make the signature extensible
/// (e.g. streaming policy, rate limit hints) without breaking call sites.
pub struct MessageContext {
    pub thread_channel: ChannelRef,
    pub sender_json: String,
    pub prompt: String,
    pub extra_blocks: Vec<ContentBlock>,
    pub trigger_msg: MessageRef,
    pub other_bot_present: bool,
}

/// Sender identity injected into prompts for downstream agent context.
///
/// This is **metadata for the agent** — `channel_id` always refers to the
/// logical parent channel, and `thread_id` identifies the thread (if any).
/// This convention is consistent across platforms (Slack, Discord, Telegram).
///
/// Compare with `ChannelRef`, which is used for **routing**: there
/// `channel_id` is the ID the adapter sends messages to (for Discord
/// threads, that's the thread's own channel ID, not the parent).
#[derive(Clone, Debug, Serialize)]
pub struct SenderContext {
    pub schema: String,
    pub sender_id: String,
    pub sender_name: String,
    pub display_name: String,
    pub channel: String,
    pub channel_id: String,
    /// Thread identifier, if the message is inside a thread.
    /// Slack: thread_ts. Discord: thread channel ID (channel_id holds the parent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub is_bot: bool,
    /// Platform message creation time (ISO 8601 UTC), if available.
    /// Discord/Slack: platform timestamp. Gateway: broker receive time (best-effort).
    /// Additive optional field — schema version stays openab.sender.v1 (no consumer
    /// breakage). If future additions require breaking changes, bump to v1.1+.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Platform message ID. Agents can use this to reply to a specific message
    /// via the `[[reply_to:<message_id>]]` output directive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The platform user ID of the receiving bot/agent.
    /// Enables agents to identify themselves when multiple agents share the same backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_id: Option<String>,
}

// --- ChatAdapter trait ---

#[async_trait]
pub trait ChatAdapter: Send + Sync + 'static {
    /// Platform name for logging and session key namespacing.
    fn platform(&self) -> &'static str;

    /// Maximum message length (chars) for this platform; the router splits longer
    /// replies into multiple messages at this bound. Platform-specific (e.g. 2000
    /// for Discord; Slack uses its Block Kit `markdown` block cap).
    fn message_limit(&self) -> usize;

    /// Send a new message, returns a reference to the sent message.
    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef>;

    /// Create a thread from a trigger message, returns the thread channel ref.
    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef>;

    /// Add a reaction/emoji to a message.
    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;

    /// Remove a reaction/emoji from a message.
    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;

    /// Edit an existing message in-place (for streaming updates).
    /// Default: unsupported (send-once only).
    async fn edit_message(&self, _msg: &MessageRef, _content: &str) -> Result<()> {
        Err(anyhow::anyhow!("edit_message not supported"))
    }

    /// Send a message as a reply to a specific message (Discord: message_reference).
    /// Default: falls back to plain send_message (ignores reply_to).
    async fn send_message_with_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        reply_to_message_id: &str,
    ) -> Result<MessageRef> {
        let _ = reply_to_message_id; // unused in default impl
        self.send_message(channel, content).await
    }

    /// Rename the thread/channel title. Default: no-op (not all platforms support it).
    async fn rename_thread(&self, _channel: &ChannelRef, _title: &str) -> Result<()> {
        Ok(())
    }

    /// Delete a message. Used to remove streaming placeholders when reply_to is set.
    /// Default: edits to zero-width space (fallback for platforms without delete support).
    async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
        self.edit_message(msg, "\u{200b}").await
    }

    /// Whether this adapter streams via a native streaming API (Slack
    /// chat.startStream) rather than the post+edit loop. Default: false.
    /// `other_bot_present` lets adapters fall back to send-once in multi-bot
    /// threads (mirrors `use_streaming`'s #534 rule).
    fn uses_native_streaming(&self, _other_bot_present: bool) -> bool {
        false
    }

    /// Begin a native stream. The returned MessageRef is the handle for
    /// subsequent `stream_append` / `stream_finish`.
    /// Default: delegate to send_message (only called when uses_native_streaming).
    /// `recipient` is the per-turn `(user_id, team_id)` for platforms (Slack) that
    /// need it for the native stream open; ignored by the default impl.
    async fn stream_begin(
        &self,
        channel: &ChannelRef,
        _recipient: Option<(String, String)>,
    ) -> Result<MessageRef> {
        self.send_message(channel, "…").await
    }

    /// Append an INCREMENTAL delta to a native stream.
    /// Default: best-effort edit (only called when uses_native_streaming).
    async fn stream_append(&self, msg: &MessageRef, delta: &str) -> Result<()> {
        self.edit_message(msg, delta).await
    }

    /// Finish a native stream and write the COMPLETE final content.
    /// Default: delegate to edit_message.
    async fn stream_finish(&self, msg: &MessageRef, final_content: &str) -> Result<()> {
        self.edit_message(msg, final_content).await
    }

    /// Whether this adapter uses a status API (e.g. assistant.threads.setStatus)
    /// instead of emoji reactions for thinking/tool indicators. Independent of
    /// `uses_native_streaming` — status can work without content streaming.
    /// Default: false.
    fn uses_assistant_status(&self) -> bool {
        false
    }

    /// Set an ephemeral status line (e.g. "Thinking…", "Using <tool>…").
    /// Empty string clears it. Default: no-op (platforms without a status API).
    async fn set_status(&self, _channel: &ChannelRef, _status: &str) -> Result<()> {
        Ok(())
    }

    /// Whether this platform renders Markdown tables natively. When `true`, the
    /// router skips the `convert_tables` pre-pass (which rewrites tables into
    /// code blocks / bullet lists for platforms that cannot render them) and
    /// lets the platform render the raw Markdown table itself.
    /// Default: `false` (keep converting). Overridden by Slack (Block Kit
    /// `markdown` blocks / `markdown_text` stream chunks render tables natively).
    fn renders_native_tables(&self) -> bool {
        false
    }

    /// Whether this adapter should use streaming edit (true) or send-once (false).
    /// `other_bot_present` indicates if another bot has posted in the current thread.
    /// Streaming should be disabled in multi-bot threads to avoid edit interference.
    /// NOTE: Slight race window exists — the multibot cache is checked before
    /// handle_message, so a bot arriving between the check and the response will
    /// not be detected until the next message. This is acceptable: the first
    /// response may stream, but subsequent ones will correctly use send-once.
    fn use_streaming(&self, other_bot_present: bool) -> bool;

    /// Whether to send the "…" placeholder message before streaming starts.
    /// Default: true. Platforms using drafts (e.g. Telegram Rich Messages) can
    /// return false to suppress the placeholder.
    fn show_streaming_placeholder(&self) -> bool {
        true
    }
}

// --- AdapterRouter ---

/// Shared logic for routing messages to ACP agents, managing sessions,
/// streaming edits, and controlling reactions. Platform-independent.
pub struct AdapterRouter {
    pool: Arc<SessionPool>,
    reactions_config: ReactionsConfig,
    table_mode: TableMode,
    prompt_hard_timeout: std::time::Duration,
    /// Polling cadence for the recv-loop liveness check (#732).
    liveness_check_interval: std::time::Duration,
    /// Workspace aliases from `[workspace.aliases]` config.
    workspace_aliases: std::collections::HashMap<String, String>,
    /// Bot home directory (security boundary for workspace directives).
    bot_home: std::path::PathBuf,
}

impl AdapterRouter {
    pub fn new(
        pool: Arc<SessionPool>,
        reactions_config: ReactionsConfig,
        table_mode: TableMode,
        prompt_hard_timeout_secs: u64,
        liveness_check_secs: u64,
        workspace_aliases: std::collections::HashMap<String, String>,
        bot_home: std::path::PathBuf,
    ) -> Self {
        if liveness_check_secs >= prompt_hard_timeout_secs {
            warn!(
                liveness_check_secs,
                prompt_hard_timeout_secs,
                "pool.liveness_check_secs >= pool.prompt_hard_timeout_secs; \
                 the hard ceiling will only fire after the next liveness tick \
                 and may be effectively bypassed. Lower liveness_check_secs."
            );
        }
        Self {
            pool,
            reactions_config,
            table_mode,
            prompt_hard_timeout: std::time::Duration::from_secs(prompt_hard_timeout_secs),
            liveness_check_interval: std::time::Duration::from_secs(liveness_check_secs),
            workspace_aliases,
            bot_home,
        }
    }

    /// Access the underlying session pool (e.g. for config option queries).
    pub fn pool(&self) -> &Arc<SessionPool> {
        &self.pool
    }

    /// Access the reactions config (used by dispatch.rs).
    pub fn reactions_config(&self) -> &ReactionsConfig {
        &self.reactions_config
    }

    /// Workspace aliases for control directive resolution.
    pub fn workspace_aliases_map(&self) -> std::collections::HashMap<String, String> {
        self.workspace_aliases.clone()
    }

    /// Bot home path for workspace security boundary.
    pub fn bot_home_path(&self) -> std::path::PathBuf {
        self.bot_home.clone()
    }

    /// Pack one arrival event into ContentBlocks. Per-arrival layout:
    ///   Text { "<sender_context>\n{json}\n</sender_context>" }   <- delimiter
    ///   [Text blocks from extra_blocks (e.g. STT transcripts)]
    ///   Text { "{prompt}" }                                       <- omitted if empty
    ///   [non-Text blocks from extra_blocks (e.g. Image)]
    ///
    /// The sender_context block stands alone so it can serve as a structural
    /// delimiter between arrivals in batched dispatch — agents can scan for
    /// `<sender_context>` openers to find arrival boundaries. Within an arrival,
    /// transcript text precedes the typed prompt to match pre-batching adapter
    /// behavior (voice content first), and images trail the prompt as before.
    /// This is the single packing code path for both per-message and batched
    /// dispatch (ADR §3.5). For a batch of N messages, call this N times and
    /// concatenate.
    pub fn pack_arrival_event(
        sender_json: &str,
        prompt: &str,
        extra_blocks: Vec<ContentBlock>,
    ) -> Vec<ContentBlock> {
        let header = format!("<sender_context>\n{}\n</sender_context>", sender_json);
        let (texts, others): (Vec<_>, Vec<_>) = extra_blocks
            .into_iter()
            .partition(|b| matches!(b, ContentBlock::Text { .. }));
        let mut blocks = Vec::with_capacity(2 + texts.len() + others.len());
        blocks.push(ContentBlock::Text { text: header });
        blocks.extend(texts);
        if !prompt.is_empty() {
            blocks.push(ContentBlock::Text {
                text: prompt.to_string(),
            });
        }
        blocks.extend(others);
        blocks
    }

    /// Handle an incoming user message. The adapter is responsible for
    /// filtering, resolving the thread, and building the SenderContext.
    /// This method handles sender context injection, session management, and streaming.
    pub async fn handle_message(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        ctx: MessageContext,
    ) -> Result<()> {
        tracing::debug!(platform = adapter.platform(), "processing message");

        let content_blocks =
            Self::pack_arrival_event(&ctx.sender_json, &ctx.prompt, ctx.extra_blocks);

        let thread_key = format!(
            "{}:{}",
            adapter.platform(),
            ctx.thread_channel
                .thread_id
                .as_deref()
                .unwrap_or(&ctx.thread_channel.channel_id)
        );

        if let Err(e) = self.pool.get_or_create(&thread_key, None).await {
            let msg = format_user_error(&e.to_string());
            let _ = adapter
                .send_message(&ctx.thread_channel, &format!("⚠️ {msg}"))
                .await;
            error!("pool error: {e}");
            return Err(e);
        }

        // In assistant-status mode (e.g. Slack assistant_mode), status is conveyed
        // via assistant.threads.setStatus, so the emoji-reaction lifecycle is skipped
        // entirely — mirrors dispatch_batch so per-message and batched modes agree.
        let assistant_status = adapter.uses_assistant_status();

        let reactions = Arc::new(StatusReactionController::new(
            self.reactions_config.enabled,
            adapter.clone(),
            ctx.trigger_msg.clone(),
            self.reactions_config.emojis.clone(),
            self.reactions_config.timing.clone(),
        ));
        if !assistant_status {
            reactions.set_queued().await;
        }

        let result = self
            .stream_prompt(
                adapter,
                &thread_key,
                content_blocks,
                &ctx.thread_channel,
                reactions.clone(),
                ctx.other_bot_present,
            )
            .await;

        if !assistant_status {
            match &result {
                Ok(()) => reactions.set_done().await,
                Err(_) => reactions.set_error().await,
            }

            let hold_ms = if result.is_ok() {
                self.reactions_config.timing.done_hold_ms
            } else {
                self.reactions_config.timing.error_hold_ms
            };
            if self.reactions_config.remove_after_reply {
                let reactions = reactions;
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
                    reactions.clear().await;
                });
            }
        }

        if let Err(ref e) = result {
            let _ = adapter
                .send_message(&ctx.thread_channel, &format!("⚠️ {e}"))
                .await;
        }

        result
    }

    async fn stream_prompt(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        thread_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
        other_bot_present: bool,
    ) -> Result<()> {
        self.stream_prompt_blocks(
            adapter,
            thread_key,
            content_blocks,
            thread_channel,
            reactions,
            other_bot_present,
            // handle_message path (e.g. cron) is never Slack assistant-mode native
            // streaming, so no per-turn recipient — degrades to post+edit if it were.
            None,
        )
        .await
    }

    /// Drive one ACP turn with the given pre-packed ContentBlocks.
    /// Called by both `handle_message` (per-message mode) and `dispatch::dispatch_batch`
    /// (batched mode).
    #[allow(clippy::too_many_arguments)]
    pub async fn stream_prompt_blocks(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        thread_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
        other_bot_present: bool,
        recipient: Option<(String, String)>,
    ) -> Result<()> {
        let adapter = adapter.clone();
        let thread_channel = thread_channel.clone();
        let message_limit = adapter.message_limit();
        let streaming = adapter.use_streaming(other_bot_present);
        // Keep the full turn text (incl. inter-tool narration) when streaming
        // (it was already shown live) OR when `[reactions] narration_display` is
        // set. Otherwise a send-once turn delivers only the final answer block.
        // Platform-agnostic — read from the shared reactions config, alongside
        // `tool_display`. `streaming` still drives the placeholder / native-stream
        // paths below; only the final-text selection uses `keep_full_text`.
        let keep_full_text = streaming || self.reactions_config.narration_display;
        let native = adapter.uses_native_streaming(other_bot_present);
        let assistant_status = adapter.uses_assistant_status();
        // Platforms that render Markdown tables natively (e.g. Slack Block Kit
        // `markdown` blocks / `markdown_text` stream chunks) skip the
        // table→code/bullets pre-pass so the raw table renders natively.
        let table_mode = if adapter.renders_native_tables() {
            TableMode::Off
        } else {
            self.table_mode
        };
        let tool_display = self.reactions_config.tool_display;
        let prompt_hard_timeout = self.prompt_hard_timeout;
        let liveness_check_interval = self.liveness_check_interval;

        self.pool
            .with_connection(thread_key, |conn| {
                let content_blocks = content_blocks.clone();
                Box::pin(async move {
                    let reset = conn.session_reset;
                    conn.session_reset = false;

                    let (mut rx, request_id) = conn.session_prompt(content_blocks).await?;
                    if assistant_status {
                        let _ = adapter.set_status(&thread_channel, "Thinking…").await;
                    } else {
                        reactions.set_thinking().await;
                    }

                    let mut text_buf = String::new();
                    let mut tool_lines: Vec<ToolEntry> = Vec::new();
                    // Byte offset into `text_buf` where the final answer block
                    // begins — advanced to the buffer end on every tool
                    // completion so it tracks "just past the last tool". Used by
                    // send-once mode to drop inter-tool narration (see
                    // `select_delivery_text`).
                    let mut answer_start = 0usize;

                    if reset {
                        text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
                    }

                    // Native streaming: defer stream_begin until first Text event
                    // so the thinking phase only shows set_status (no placeholder msg).
                    let mut native_msg: Option<MessageRef> = None;
                    // Once stream_begin fails, stop retrying for this turn to avoid
                    // hammering the API on transient failures.
                    let mut stream_begin_failed = false;
                    // Native delta coalescing state (used only when `native`).
                    let mut native_pending = String::new();
                    let mut native_last_flush = tokio::time::Instant::now();
                    const NATIVE_FLUSH_MS: u128 = 400;

                    // Streaming edit: send placeholder, spawn edit loop
                    let (buf_tx, placeholder_msg, edit_handle) = if streaming && !native {
                        let initial = if reset {
                            "⚠️ _Session expired, starting fresh..._\n\n…".to_string()
                        } else {
                            "…".to_string()
                        };
                        let msg = if adapter.show_streaming_placeholder() {
                            adapter.send_message(&thread_channel, &initial).await?
                        } else {
                            // Dummy ref for edit loop — gateway uses drafts, doesn't need real msg_id
                            MessageRef {
                                message_id: "draft".to_string(),
                                channel: thread_channel.clone(),
                            }
                        };
                        let (tx, rx) = tokio::sync::watch::channel(initial);
                        let edit_adapter = adapter.clone();
                        let edit_msg = msg.clone();
                        let limit = message_limit;
                        let mut buf_rx = rx;
                        let edit_handle = tokio::spawn(async move {
                            let mut last = String::new();
                            // Track consecutive edit failures so we can abort cosmetic
                            // streaming when the platform stops accepting edits (e.g.
                            // Feishu's 20-edits-per-message hard cap, errcode 230072).
                            // Once aborted, the final delivery path still runs and the
                            // user sees the complete content at turn end.
                            let mut consecutive_failures: u32 = 0;
                            const MAX_CONSECUTIVE_FAILURES: u32 = 3;
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                                if buf_rx.has_changed().unwrap_or(false) {
                                    let content = buf_rx.borrow_and_update().clone();
                                    if content != last {
                                        let display = if content.chars().count() > limit - 100 {
                                            format!(
                                                "…{}",
                                                format::truncate_chars_tail(&content, limit - 100)
                                            )
                                        } else {
                                            content.clone()
                                        };
                                        match edit_adapter
                                            .edit_message(&edit_msg, &display)
                                            .await
                                        {
                                            Ok(_) => {
                                                consecutive_failures = 0;
                                                last = content;
                                            }
                                            Err(e) => {
                                                consecutive_failures += 1;
                                                tracing::debug!(
                                                    message_id = %edit_msg.message_id,
                                                    platform = %edit_msg.channel.platform,
                                                    error = ?e,
                                                    consecutive_failures,
                                                    "mid-stream cosmetic edit failed"
                                                );
                                                if consecutive_failures
                                                    >= MAX_CONSECUTIVE_FAILURES
                                                {
                                                    tracing::warn!(
                                                        message_id = %edit_msg.message_id,
                                                        platform = %edit_msg.channel.platform,
                                                        consecutive_failures,
                                                        "mid-stream cosmetic edit aborted; \
                                                         final content will be delivered at turn end"
                                                    );
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                if buf_rx.has_changed().is_err() {
                                    break;
                                }
                            }
                        });
                        (Some(tx), Some(msg), Some(edit_handle))
                    } else {
                        (None, None, None)
                    };

                    // (#732) Liveness-aware recv loop. Filters stale id-bearing
                    // messages and abandons cleanly on dead agent / hard ceiling
                    // so late responses cannot leak into the next prompt.
                    let mut response_error: Option<String> = None;
                    let mut turn_result = TurnResult::default();
                    let prompt_start = tokio::time::Instant::now();
                    loop {
                        let notification = tokio::select! {
                            msg = rx.recv() => match msg {
                                Some(n) => n,
                                // Reader saw EOF: the agent's stdout closed. A *successful*
                                // turn is always signalled by the id-bearing JSON-RPC response to
                                // `session/prompt`, which breaks the loop at the id branch below
                                // *before* any EOF — so reaching this arm means the turn ended
                                // without a final response, i.e. the agent terminated abnormally
                                // (bridged agents that crash on a backend error such as HTTP 500 /
                                // quota exhausted exit without ever emitting an ACP error
                                // notification). Surface that as an explicit error instead of
                                // falling through to "_(no response)_" or, worse, presenting a
                                // partially-streamed buffer as a complete answer.
                                //
                                // Do NOT gate on `text_buf.is_empty()`: the buffer is pre-seeded
                                // on session reset (the expiry notice) and, in send-once mode,
                                // carries inter-tool narration that is sliced off before delivery —
                                // so a non-empty buffer is not evidence the turn completed. When
                                // partial text *was* streamed, `final_content` prepends the warning
                                // to it (⚠️ … \n\n <partial>), preserving the output while flagging
                                // the truncation.
                                None => {
                                    if response_error.is_none() {
                                        response_error =
                                            Some("Agent process exited unexpectedly".into());
                                    }
                                    break;
                                }
                            },
                            _ = tokio::time::sleep(liveness_check_interval) => {
                                if !conn.alive() {
                                    response_error = Some("Agent process died".into());
                                    conn.abandon_request(request_id).await;
                                    break;
                                }
                                if prompt_start.elapsed() > prompt_hard_timeout {
                                    response_error = Some(format!(
                                        "Agent exceeded hard timeout ({}s)",
                                        prompt_hard_timeout.as_secs(),
                                    ));
                                    conn.abandon_request(request_id).await;
                                    break;
                                }
                                continue;
                            }
                        };
                        if let Some(notification_id) = notification.id {
                            if notification_id != request_id {
                                // Stale response from a previously-abandoned prompt.
                                // No automated test seam: this path only triggers when a
                                // real subprocess emits a late response after the broker
                                // already called abandon_request — covered by manual
                                // repro against a live agent (see #732 PR description).
                                continue;
                            }
                            if let Some(ref err) = notification.error {
                                response_error = Some(format_coded_error(err.code, &err.message, err.data_message()));
                            }
                            if let Some(ref result) = notification.result {
                                turn_result = parse_turn_result(result);
                            }
                            break;
                        }

                        if let Some(event) = classify_notification(&notification) {
                            match event {
                                AcpEvent::Text(t) => {
                                    text_buf.push_str(&t);
                                    if native {
                                        // Lazy stream_begin: open the stream on first text.
                                        if native_msg.is_none() && !stream_begin_failed {
                                            match adapter.stream_begin(&thread_channel, recipient.clone()).await {
                                                Ok(m) => { native_msg = Some(m); }
                                                Err(e) => {
                                                    tracing::error!(error = ?e, "stream_begin failed on first text; will not retry this turn");
                                                    stream_begin_failed = true;
                                                }
                                            }
                                        }
                                        if let Some(msg) = &native_msg {
                                            native_pending.push_str(&t);
                                            if native_last_flush.elapsed().as_millis()
                                                >= NATIVE_FLUSH_MS
                                                && !native_pending.is_empty()
                                            {
                                                let _ = adapter
                                                    .stream_append(msg, &native_pending)
                                                    .await;
                                                native_pending.clear();
                                                native_last_flush = tokio::time::Instant::now();
                                            }
                                        }
                                    } else if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(
                                            &tool_lines,
                                            &text_buf,
                                            true,
                                            tool_display,
                                        ));
                                    }
                                }
                                AcpEvent::Thinking => {
                                    if assistant_status {
                                        let _ = adapter
                                            .set_status(&thread_channel, "Thinking…")
                                            .await;
                                    } else {
                                        reactions.set_thinking().await;
                                    }
                                }
                                AcpEvent::ToolStart { id, title } if !title.is_empty() => {
                                    // Live indicator: assistant status line vs emoji reaction.
                                    if assistant_status {
                                        let _ = adapter
                                            .set_status(
                                                &thread_channel,
                                                &format!("Using {title}…"),
                                            )
                                            .await;
                                    } else {
                                        reactions.set_tool(&title).await;
                                    }
                                    // Record the tool in BOTH modes so the finalized message keeps
                                    // a tool summary (compose_display, gated by tool_display). In
                                    // assistant_mode the status line is transient and cleared before
                                    // the reply, so without this the message would retain no record
                                    // of which tools ran.
                                    let title = sanitize_title(&title);
                                    if let Some(slot) =
                                        tool_lines.iter_mut().find(|e| e.id == id)
                                    {
                                        slot.title = title;
                                        slot.state = ToolState::Running;
                                    } else {
                                        tool_lines.push(ToolEntry {
                                            id,
                                            title,
                                            state: ToolState::Running,
                                        });
                                    }
                                    // Post+edit live update (no-op under native streaming: buf_tx is None).
                                    if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(
                                            &tool_lines,
                                            &text_buf,
                                            true,
                                            tool_display,
                                        ));
                                    }
                                }
                                AcpEvent::ToolDone { id, title, status } => {
                                    // The final answer block is whatever text the agent
                                    // emits AFTER its last tool. Advancing this on every
                                    // completion leaves it pointing just past the last
                                    // tool; send-once delivery slices from here so the
                                    // preceding inter-tool narration is dropped.
                                    answer_start = text_buf.len();
                                    // Live indicator: assistant status line vs emoji reaction.
                                    if assistant_status {
                                        let _ = adapter
                                            .set_status(&thread_channel, "Thinking…")
                                            .await;
                                    } else {
                                        reactions.set_thinking().await;
                                    }
                                    // Update the tool's state in BOTH modes (see ToolStart) so the
                                    // finalized message's tool summary reflects completion/failure.
                                    let new_state = if status == "completed" {
                                        ToolState::Completed
                                    } else {
                                        ToolState::Failed
                                    };
                                    if let Some(slot) =
                                        tool_lines.iter_mut().find(|e| e.id == id)
                                    {
                                        if !title.is_empty() {
                                            slot.title = sanitize_title(&title);
                                        }
                                        slot.state = new_state;
                                    } else if !title.is_empty() {
                                        tool_lines.push(ToolEntry {
                                            id,
                                            title: sanitize_title(&title),
                                            state: new_state,
                                        });
                                    }
                                    if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(
                                            &tool_lines,
                                            &text_buf,
                                            true,
                                            tool_display,
                                        ));
                                    }
                                }
                                AcpEvent::ConfigUpdate { options } => {
                                    conn.config_options = options;
                                }
                                _ => {}
                            }
                        }
                    }

                    conn.prompt_done().await;
                    // Stop the cosmetic edit loop before the finalize write path
                    // issues its authoritative edit. Dropping buf_tx closes the watch
                    // channel so the loop breaks on its next check, but it may be
                    // mid-edit (a single edit can now block up to the gateway response
                    // timeout). Without an explicit abort+join, a cosmetic edit issued
                    // just before close could land *after* the finalize edit and
                    // overwrite it with stale, mid-stream content (#1122 review NEW-1).
                    //
                    // abort() cancels any cosmetic edit that has not yet been put on
                    // the wire and interrupts the inter-flush sleep immediately; the
                    // await confirms the task is gone before we proceed. This narrows
                    // the race to near zero — it does NOT fully eliminate it: a PUT
                    // already flushed microseconds before abort cannot be recalled,
                    // and if finalize's PUT travels a different pooled connection the
                    // server-side arrival order is not strictly guaranteed. That
                    // residual window is display-only (stale tail briefly shown) and
                    // far narrower than before this join existed.
                    drop(buf_tx);
                    if let Some(handle) = edit_handle {
                        handle.abort();
                        let _ = handle.await;
                    }

                    // In send-once mode, deliver only the final answer block —
                    // the text after the last tool call — so inter-tool narration
                    // ("let me pull the diff", "now reading X") never reaches the
                    // message. Streaming modes already showed that text live, so
                    // they keep the whole buffer. Directives are parsed from the
                    // FULL buffer (they sit at output start, which the slice may
                    // drop) so a leading [[reply_to:...]] survives the narration
                    // it was emitted alongside.
                    let (directives, text_buf) =
                        split_delivery(&text_buf, answer_start, keep_full_text);
                    // The session-reset notice lives at the head of the buffer; a
                    // tool advancing answer_start past it would drop it from the
                    // slice, so re-prepend it to the (directive-stripped) body in
                    // exactly that case. `finalize_body` is the pure helper that
                    // encodes the four-corner truth table so it can be unit-tested.
                    let text_buf = finalize_body(reset, keep_full_text, answer_start, text_buf);

                    // Build final content
                    let final_content =
                        compose_display(&tool_lines, &text_buf, false, tool_display);
                    let final_content = if final_content.is_empty() {
                        if turn_result.is_silent_failure() {
                            warn!(
                                stop_reason = ?turn_result.stop_reason,
                                input_tokens = ?turn_result.input_tokens,
                                output_tokens = ?turn_result.output_tokens,
                                total_tokens = ?turn_result.total_tokens,
                                "agent returned empty turn (0 output tokens) — likely provider/model/auth failure"
                            );
                        }
                        classify_empty_turn(response_error.as_deref(), &turn_result)
                    } else if let Some(err) = response_error {
                        format!("⚠️ {err}\n\n{final_content}")
                    } else {
                        final_content
                    };

                    let final_content = markdown::convert_tables(&final_content, table_mode);
                    let chunks = if adapter.platform() == "discord" {
                        let mentions = extract_mentions(&final_content);
                        let mention_reserve = mention_footer_len(&mentions);
                        let chunks = format::split_message(
                            &final_content,
                            message_limit.saturating_sub(mention_reserve),
                        );
                        propagate_mentions_to_chunks(chunks, &mentions, message_limit)
                    } else {
                        format::split_message(&final_content, message_limit)
                    };
                    // Track delivery health across all final write paths. Any failure
                    // here means the user's view is incomplete; we propagate Err at the
                    // end of the closure so dispatch surfaces set_error (❌) instead of
                    // silently calling set_done (🆗) over a half-delivered turn.
                    let mut delivery_failed = false;
                    // Clear the assistant status line before delivering the final message.
                    if assistant_status {
                        let _ = adapter.set_status(&thread_channel, "").await;
                    }
                    if native {
                        if let Some(msg) = &native_msg {
                            if !native_pending.is_empty() {
                                if let Err(e) =
                                    adapter.stream_append(msg, &native_pending).await
                                {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "native finalize stream_append failed");
                                    delivery_failed = true;
                                }
                            }
                            // Finalize the streamed message with the first chunk (full-replace),
                            // then post any overflow chunks as new in-thread messages — mirrors
                            // the post+edit path so long replies aren't truncated at message_limit.
                            // NOTE: the reply_to directive is intentionally NOT honored in native
                            // streaming mode — the streamed message is the in-thread reply.
                            match chunks.first() {
                                Some(first) => {
                                    if let Err(e) = adapter.stream_finish(msg, first).await {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "native stream_finish failed");
                                        delivery_failed = true;
                                    }
                                    for chunk in chunks.iter().skip(1) {
                                        if let Err(e) =
                                            adapter.send_message(&thread_channel, chunk).await
                                        {
                                            tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "native overflow chunk send failed");
                                            delivery_failed = true;
                                        }
                                    }
                                }
                                None => {
                                    if let Err(e) =
                                        adapter.stream_finish(msg, &final_content).await
                                    {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "native stream_finish (no chunks) failed");
                                        delivery_failed = true;
                                    }
                                }
                            }
                        } else {
                            // native_msg is None — either no Text event ever arrived
                            // (tool-only or empty turn) so lazy stream_begin never
                            // fired, or stream_begin failed on the first Text event
                            // and we stopped retrying for this turn. In both cases no
                            // native stream was opened, so deliver the final content
                            // (which may be the "_(no response)_" sentinel, or the
                            // accumulated text_buf) as plain in-thread messages so
                            // the turn is never silently dropped.
                            for chunk in &chunks {
                                if let Err(e) =
                                    adapter.send_message(&thread_channel, chunk).await
                                {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, "native fallback chunk send failed");
                                    delivery_failed = true;
                                }
                            }
                        }
                    } else if let Some(msg) = placeholder_msg {
                        if let Some(ref reply_id) = directives.reply_to {
                            // reply_to directive: send reply first, then delete placeholder.
                            // Only delete if send succeeds — preserves placeholder on failure.
                            let mut send_ok = false;
                            let mut first = true;
                            for chunk in &chunks {
                                if first {
                                    match adapter.send_message_with_reply(
                                        &thread_channel,
                                        chunk,
                                        reply_id,
                                    ).await {
                                        Ok(_) => { send_ok = true; }
                                        Err(e) => {
                                            tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "reply_to send failed; preserving placeholder");
                                            delivery_failed = true;
                                        }
                                    }
                                } else if let Err(e) =
                                    adapter.send_message(&thread_channel, chunk).await
                                {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "reply_to overflow chunk send failed");
                                    delivery_failed = true;
                                }
                                first = false;
                            }
                            if send_ok {
                                if let Err(e) = adapter.delete_message(&msg).await {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "delete placeholder failed; placeholder will remain visible");
                                }
                            }
                        } else if adapter.platform() == "discord"
                            && contains_bot_mention(&final_content)
                        {
                            // Discord-specific: bot mention detected. Delete placeholder
                            // and send as new message so Discord emits MESSAGE_CREATE —
                            // otherwise the mentioned bot won't receive the gateway
                            // event since MESSAGE_UPDATE skips notifications (#1110).
                            let mut send_ok = false;
                            if let Some(first) = chunks.first() {
                                match adapter.send_message(&thread_channel, first).await {
                                    Ok(_) => {
                                        send_ok = true;
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "discord bot-mention first chunk send failed");
                                        delivery_failed = true;
                                    }
                                }
                            }
                            for chunk in chunks.iter().skip(1) {
                                if let Err(e) = adapter.send_message(&thread_channel, chunk).await {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "streaming overflow chunk send failed");
                                    delivery_failed = true;
                                }
                            }
                            if send_ok {
                                let _ = adapter.delete_message(&msg).await;
                            }
                        } else {
                            // Normal streaming: edit first chunk into placeholder, send rest.
                            // If placeholder is a dummy "draft" ref (no real message), send as
                            // new message instead — the gateway will persist via sendRichMessage.
                            if msg.message_id == "draft" {
                                for chunk in &chunks {
                                    if let Err(e) =
                                        adapter.send_message(&thread_channel, chunk).await
                                    {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "draft placeholder fallback chunk send failed");
                                        delivery_failed = true;
                                    }
                                }
                            } else if let Some(first) = chunks.first() {
                                // If the placeholder edit fails (e.g. Feishu's
                                // 20-edits-per-message cap was hit during
                                // cosmetic streaming and the gateway reports
                                // edit_cap_reached), fall back to deleting the
                                // half-edited placeholder and sending the first
                                // chunk as a fresh message so the user sees the
                                // complete reply without overlap. If delete
                                // fails the placeholder simply remains — same
                                // UX as pre-recovery, not a hard failure.
                                if let Err(e) = adapter.edit_message(&msg, first).await {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "final streaming edit failed; deleting placeholder and sending fresh");
                                    if let Err(de) = adapter.delete_message(&msg).await {
                                        tracing::warn!(error = ?de, platform = %thread_channel.platform, message_id = %msg.message_id, "delete placeholder failed; user will see overlap");
                                    }
                                    if let Err(e2) =
                                        adapter.send_message(&thread_channel, first).await
                                    {
                                        tracing::error!(error = ?e2, platform = %thread_channel.platform, message_id = %msg.message_id, "fallback send_message also failed");
                                        delivery_failed = true;
                                    }
                                }
                                for chunk in chunks.iter().skip(1) {
                                    if let Err(e) =
                                        adapter.send_message(&thread_channel, chunk).await
                                    {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, message_id = %msg.message_id, "streaming overflow chunk send failed");
                                        delivery_failed = true;
                                    }
                                }
                            }
                        }
                    } else {
                        // Send-once: all chunks as new messages
                        // First chunk uses reply_to directive if present
                        let mut first = true;
                        for chunk in &chunks {
                            if first {
                                if let Some(ref reply_id) = directives.reply_to {
                                    if let Err(e) = adapter.send_message_with_reply(
                                        &thread_channel,
                                        chunk,
                                        reply_id,
                                    ).await {
                                        tracing::warn!(error = ?e, platform = %thread_channel.platform, "send-once reply_to first chunk failed");
                                        delivery_failed = true;
                                    }
                                } else if let Err(e) =
                                    adapter.send_message(&thread_channel, chunk).await
                                {
                                    tracing::warn!(error = ?e, platform = %thread_channel.platform, "send-once first chunk failed");
                                    delivery_failed = true;
                                }
                            } else if let Err(e) =
                                adapter.send_message(&thread_channel, chunk).await
                            {
                                tracing::warn!(error = ?e, platform = %thread_channel.platform, "send-once subsequent chunk failed");
                                delivery_failed = true;
                            }
                            first = false;
                        }
                    }

                    if delivery_failed {
                        Err(anyhow::anyhow!(
                            "streaming finalization had delivery failures; user view is incomplete"
                        ))
                    } else {
                        Ok(())
                    }
                })
            })
            .await
    }
}

/// Returns true if `content` contains a Discord user/bot mention (`<@123>`, `<@!123>`)
/// or a role mention (`<@&123>`).
/// Used to detect cross-bot mentions so the streaming path can switch from
/// edit (MESSAGE_UPDATE, no mention notification) to delete+send (MESSAGE_CREATE).
fn contains_bot_mention(content: &str) -> bool {
    let mut i = 0;
    let bytes = content.as_bytes();
    while i + 2 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'@' {
            // Skip optional '!' (nickname mention) or '&' (role mention)
            let start = if i + 2 < bytes.len()
                && (bytes[i + 2] == b'!' || bytes[i + 2] == b'&')
            {
                i + 3
            } else {
                i + 2
            };
            if start < bytes.len() && bytes[start].is_ascii_digit() {
                if let Some(end) = content[start..].find('>') {
                    if content[start..start + end].chars().all(|c| c.is_ascii_digit()) {
                        return true;
                    }
                }
            }
            i = start;
        } else {
            i += 1;
        }
    }
    false
}

/// Flatten a tool-call title into a single line safe for inline-code spans.
fn sanitize_title(title: &str) -> String {
    title
        .replace('\r', "")
        .replace('\n', " ; ")
        .replace('`', "'")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
struct ToolEntry {
    id: String,
    title: String,
    state: ToolState,
}

impl ToolEntry {
    fn render(&self) -> String {
        let icon = match self.state {
            ToolState::Running => "🔧",
            ToolState::Completed => "✅",
            ToolState::Failed => "❌",
        };
        let suffix = if self.state == ToolState::Running {
            "..."
        } else {
            ""
        };
        format!("{icon} `{}`{}", self.title, suffix)
    }
}

/// Maximum number of finished tool entries to show individually
/// during streaming before collapsing into a summary line.
const TOOL_COLLAPSE_THRESHOLD: usize = 3;

// --- Empty-turn classification (pure helper, unit-testable) ---

/// Message to show the consumer when a silent failure is detected.
pub(crate) const SILENT_FAILURE_MSG: &str = "⚠️ The agent did not produce a response. This usually indicates a backend configuration issue — not an intentional empty reply. Please try again later.";

/// Classify what to display when the composed body is empty.
/// Returns the final content string for the consumer.
pub(crate) fn classify_empty_turn(
    response_error: Option<&str>,
    turn_result: &TurnResult,
) -> String {
    if let Some(err) = response_error {
        format!("⚠️ {err}")
    } else if turn_result.is_silent_failure() {
        SILENT_FAILURE_MSG.to_string()
    } else {
        "_(no response)_".to_string()
    }
}

fn compose_display(
    tool_lines: &[ToolEntry],
    text: &str,
    streaming: bool,
    tool_display: ToolDisplay,
) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() && tool_display != ToolDisplay::None {
        let done = tool_lines
            .iter()
            .filter(|e| e.state == ToolState::Completed)
            .count();
        let failed = tool_lines
            .iter()
            .filter(|e| e.state == ToolState::Failed)
            .count();
        let running = tool_lines
            .iter()
            .filter(|e| e.state == ToolState::Running)
            .count();
        let finished = done + failed;

        match tool_display {
            ToolDisplay::Compact => {
                // Always show count summary, never per-tool details
                let mut parts = Vec::new();
                if done > 0 {
                    parts.push(format!("✅ {done}"));
                }
                if failed > 0 {
                    parts.push(format!("❌ {failed}"));
                }
                if running > 0 {
                    parts.push(format!("🔧 {running}"));
                }
                if !parts.is_empty() {
                    out.push_str(&format!("{} tool(s)\n", parts.join(" · ")));
                }
            }
            ToolDisplay::Full => {
                if streaming {
                    let running_entries: Vec<_> = tool_lines
                        .iter()
                        .filter(|e| e.state == ToolState::Running)
                        .collect();

                    if finished <= TOOL_COLLAPSE_THRESHOLD {
                        for entry in tool_lines.iter().filter(|e| e.state != ToolState::Running) {
                            out.push_str(&entry.render());
                            out.push('\n');
                        }
                    } else {
                        let mut parts = Vec::new();
                        if done > 0 {
                            parts.push(format!("✅ {done}"));
                        }
                        if failed > 0 {
                            parts.push(format!("❌ {failed}"));
                        }
                        out.push_str(&format!("{} tool(s) completed\n", parts.join(" · ")));
                    }

                    if running_entries.len() <= TOOL_COLLAPSE_THRESHOLD {
                        for entry in &running_entries {
                            out.push_str(&entry.render());
                            out.push('\n');
                        }
                    } else {
                        let hidden = running_entries.len() - TOOL_COLLAPSE_THRESHOLD;
                        out.push_str(&format!("🔧 {hidden} more running\n"));
                        for entry in running_entries.iter().skip(hidden) {
                            out.push_str(&entry.render());
                            out.push('\n');
                        }
                    }
                } else {
                    for entry in tool_lines {
                        out.push_str(&entry.render());
                        out.push('\n');
                    }
                }
            }
            ToolDisplay::None => {} // guarded above, but safe no-op
        }
        if !out.is_empty() {
            out.push('\n');
        }
    }
    out.push_str(text.trim_end());
    out
}

fn extract_mentions(content: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let mut in_fence = false;

    for line in content.split('\n') {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        let bytes = line.as_bytes();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i] == b'<' && bytes[i + 1] == b'@' {
                let (prefix_end, is_role) = if i + 2 < bytes.len() && bytes[i + 2] == b'&' {
                    (i + 3, true)
                } else if i + 2 < bytes.len() && bytes[i + 2] == b'!' {
                    (i + 3, false)
                } else {
                    (i + 2, false)
                };
                if prefix_end < bytes.len() && bytes[prefix_end].is_ascii_digit() {
                    if let Some(end) = line[prefix_end..].find('>') {
                        if line[prefix_end..prefix_end + end]
                            .chars()
                            .all(|c| c.is_ascii_digit())
                        {
                            let uid = &line[prefix_end..prefix_end + end];
                            let normalized = if is_role {
                                format!("<@&{uid}>")
                            } else {
                                format!("<@{uid}>")
                            };
                            if !mentions.contains(&normalized) {
                                mentions.push(normalized);
                            }
                            i = prefix_end + end + 1;
                            continue;
                        }
                    }
                }
                i = prefix_end;
            } else {
                i += 1;
            }
        }
    }
    mentions
}

fn mention_footer_len(mentions: &[String]) -> usize {
    if mentions.is_empty() {
        return 0;
    }
    1 + mentions.iter().map(|m| m.len()).sum::<usize>() + mentions.len().saturating_sub(1)
}

fn propagate_mentions_to_chunks(
    chunks: Vec<String>,
    mentions: &[String],
    limit: usize,
) -> Vec<String> {
    if mentions.is_empty() || chunks.len() <= 1 {
        return chunks;
    }
    chunks
        .into_iter()
        .map(|chunk| {
            let missing: Vec<&String> = mentions
                .iter()
                .filter(|m| !chunk.contains(m.as_str()))
                .collect();
            if missing.is_empty() {
                chunk
            } else {
                let footer = format!(
                    "\n{}",
                    missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(" ")
                );
                if chunk.chars().count() + footer.chars().count() <= limit {
                    format!("{chunk}{footer}")
                } else {
                    chunk
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_delivery_text_send_once_keeps_only_final_block() {
        // Simulates: narration "n1" → tool (answer_start→2) → narration "n2"
        // → tool (answer_start→14) → final answer. In send-once mode only the
        // text after the last tool survives.
        let full = "n1[tool]n2[tool]the final answer";
        let answer_start = "n1[tool]n2[tool]".len();
        assert_eq!(
            select_delivery_text(full, answer_start, false),
            "the final answer"
        );
    }

    #[test]
    fn select_delivery_text_streaming_keeps_full_buffer() {
        // Streaming already showed the text live, so the whole buffer is kept
        // regardless of answer_start.
        let full = "narration then answer";
        assert_eq!(select_delivery_text(full, 10, true), full);
    }

    #[test]
    fn select_delivery_text_send_once_no_tools_keeps_everything() {
        // No tool ever completed → answer_start stays 0 → the whole (tool-free)
        // reply is delivered, including a leading session-reset notice.
        let full = "⚠️ _Session expired, starting fresh..._\n\njust the answer";
        assert_eq!(select_delivery_text(full, 0, false), full);
    }

    #[test]
    fn select_delivery_text_stale_offset_falls_back_to_full() {
        // A byte offset past the end (or a non-char-boundary) must not panic —
        // get(..) returns None and we fall back to the full buffer.
        let full = "abc";
        assert_eq!(select_delivery_text(full, 999, false), full);
        // 1 is a non-boundary inside the multi-byte '✓' (3 bytes); fallback.
        assert_eq!(select_delivery_text("✓x", 1, false), "✓x");
    }

    #[test]
    fn split_delivery_send_once_preserves_leading_directive_across_tools() {
        // Regression: a [[reply_to:...]] emitted at output start, followed by
        // narration + a tool, must survive even though the narration is dropped.
        let full = "[[reply_to:101]]\nlet me check...[tool]the final answer";
        let answer_start = "[[reply_to:101]]\nlet me check...[tool]".len();
        let (directives, body) = split_delivery(full, answer_start, false);
        assert_eq!(directives.reply_to.as_deref(), Some("101"));
        assert_eq!(body, "the final answer");
    }

    #[test]
    fn split_delivery_send_once_no_tools_strips_directive_from_body() {
        // No tool ran (answer_start == 0): the slice still carries the header,
        // so the body must have it stripped while directives are still parsed.
        let full = "[[reply_to:55]]\njust the answer";
        let (directives, body) = split_delivery(full, 0, false);
        assert_eq!(directives.reply_to.as_deref(), Some("55"));
        assert_eq!(body, "just the answer");
    }

    #[test]
    fn split_delivery_streaming_keeps_full_body_and_directive() {
        // Streaming keeps the full buffer; directive parsed and stripped once.
        let full = "[[reply_to:7]]\nnarration then answer";
        let (directives, body) = split_delivery(full, 5, true);
        assert_eq!(directives.reply_to.as_deref(), Some("7"));
        assert_eq!(body, "narration then answer");
    }

    // --- finalize_body: four-corner truth table for the reset re-prepend ---
    //
    // The send-once trimming logic in `stream_prompt_blocks` ends with an
    // inline branch that decides whether to re-prepend the session-reset
    // notice. Extracted into the pure helper `finalize_body` so each corner
    // of (reset, keep_full_text, answer_start) can be exercised without a live
    // ACP session. Mirrors the integration-level concern raised in PR #1115
    // peer review (howie group-review, "Important #3").

    #[test]
    fn finalize_body_reset_send_once_with_tools_prepends_notice() {
        // Reset turn, send-once trimming, a tool advanced answer_start past
        // the notice → the slice no longer contains it → re-prepend.
        let body = "the final answer".to_string();
        let out = finalize_body(true, false, 42, body);
        assert_eq!(
            out, "⚠️ _Session expired, starting fresh..._\n\nthe final answer",
            "send-once + reset + tool ran → notice must be re-prepended"
        );
    }

    #[test]
    fn finalize_body_reset_send_once_no_tools_passes_through() {
        // answer_start == 0 means the slice still equals the full buffer,
        // which already starts with the notice → re-prepending would
        // duplicate it.
        let body = "⚠️ _Session expired, starting fresh..._\n\nthe final answer".to_string();
        let out = finalize_body(true, false, 0, body.clone());
        assert_eq!(
            out, body,
            "send-once + reset + no tools → body already carries notice, pass through"
        );
    }

    #[test]
    fn finalize_body_reset_keep_full_passes_through() {
        // keep_full_text means the slice is the whole buffer (incl. the
        // notice) → must not duplicate, regardless of answer_start.
        let body = "⚠️ _Session expired, starting fresh..._\n\nnarration then answer".to_string();
        let out = finalize_body(true, true, 42, body.clone());
        assert_eq!(
            out, body,
            "keep_full_text → body already carries notice, pass through even with tools"
        );
    }

    #[test]
    fn finalize_body_no_reset_send_once_passes_through() {
        // Non-reset turn: there is no notice to manage regardless of other flags.
        let body = "the final answer".to_string();
        assert_eq!(
            finalize_body(false, false, 42, body.clone()),
            body,
            "no reset → never prepend (send-once + tools)"
        );
    }

    #[test]
    fn finalize_body_no_reset_keep_full_passes_through() {
        // Non-reset turn with keep_full_text: notice is absent, pass through.
        let body = "the final answer".to_string();
        assert_eq!(
            finalize_body(false, true, 0, body.clone()),
            body,
            "no reset → never prepend (keep_full + no tools)"
        );
    }

    /// Compile-time regression guard: use_streaming() is a required trait method
    /// (no default). Any adapter that forgets to implement it will fail to compile.
    /// This test documents the contract — see PR #503 / issue #502 for context.
    #[test]
    fn use_streaming_is_required_method() {
        // If use_streaming() had a default impl, this test module would still
        // compile even if an adapter forgot to override it. The real guard is
        // the trait definition itself — this test exists as documentation and
        // to catch if someone re-adds a default.
        struct TestAdapter;

        #[async_trait]
        impl ChatAdapter for TestAdapter {
            fn platform(&self) -> &'static str {
                "test"
            }
            fn message_limit(&self) -> usize {
                2000
            }
            async fn send_message(&self, _: &ChannelRef, _: &str) -> Result<MessageRef> {
                unimplemented!()
            }
            async fn create_thread(
                &self,
                _: &ChannelRef,
                _: &MessageRef,
                _: &str,
            ) -> Result<ChannelRef> {
                unimplemented!()
            }
            async fn add_reaction(&self, _: &MessageRef, _: &str) -> Result<()> {
                Ok(())
            }
            async fn remove_reaction(&self, _: &MessageRef, _: &str) -> Result<()> {
                Ok(())
            }
            // use_streaming() MUST be declared — removing this line should fail compilation
            fn use_streaming(&self, _other_bot_present: bool) -> bool {
                false
            }
        }

        let adapter = TestAdapter;
        // Verify the method is callable and returns the declared value
        assert!(!adapter.use_streaming(false));
        // renders_native_tables defaults to false: platforms that don't override
        // it keep the table→code/bullets conversion (e.g. Discord, Gateway).
        assert!(!adapter.renders_native_tables());
    }

    #[test]
    fn origin_event_id_excluded_from_eq() {
        let a = ChannelRef {
            platform: "line".into(),
            channel_id: "U123".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt_aaa".into()),
        };
        let b = ChannelRef {
            platform: "line".into(),
            channel_id: "U123".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt_bbb".into()),
        };
        assert_eq!(a, b, "same channel with different event IDs must be equal");
    }

    #[test]
    fn origin_event_id_excluded_from_hash() {
        use std::collections::HashMap;
        let a = ChannelRef {
            platform: "line".into(),
            channel_id: "U123".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt_aaa".into()),
        };
        let b = ChannelRef {
            platform: "line".into(),
            channel_id: "U123".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt_bbb".into()),
        };
        let mut map = HashMap::new();
        map.insert(a, "first");
        // b should hit the same bucket and overwrite
        map.insert(b, "second");
        assert_eq!(map.len(), 1);
        assert_eq!(map.values().next(), Some(&"second"));
    }

    #[test]
    fn origin_event_id_survives_clone() {
        let ch = ChannelRef {
            platform: "line".into(),
            channel_id: "U123".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt_abc".into()),
        };
        // Simulates create_thread propagation: clone preserves origin_event_id
        let thread_ch = ChannelRef {
            thread_id: Some("topic_1".into()),
            origin_event_id: ch.origin_event_id.clone(),
            ..ch.clone()
        };
        assert_eq!(thread_ch.origin_event_id.as_deref(), Some("evt_abc"));
    }

    fn tool(id: &str, title: &str, state: ToolState) -> ToolEntry {
        ToolEntry {
            id: id.into(),
            title: title.into(),
            state,
        }
    }

    #[test]
    fn compose_display_full_shows_complete_title() {
        let tools = vec![tool(
            "1",
            "curl -s https://example.com",
            ToolState::Completed,
        )];
        let out = compose_display(&tools, "done", false, ToolDisplay::Full);
        assert!(out.contains("`curl -s https://example.com`"));
    }

    #[test]
    fn compose_display_compact_shows_count_summary() {
        let tools = vec![
            tool("1", "curl -s https://example.com", ToolState::Completed),
            tool("2", "grep -r pattern src/", ToolState::Completed),
            tool("3", "cat /etc/hosts", ToolState::Failed),
        ];
        let out = compose_display(&tools, "done", false, ToolDisplay::Compact);
        assert!(out.contains("✅ 2"), "expected completed count: {out}");
        assert!(out.contains("❌ 1"), "expected failed count: {out}");
        assert!(out.contains("tool(s)"), "expected tool(s) label: {out}");
        // Must NOT contain individual tool names
        assert!(!out.contains("curl"), "should not show tool names: {out}");
        assert!(!out.contains("grep"), "should not show tool names: {out}");
    }

    #[test]
    fn compose_display_compact_shows_running_count() {
        let tools = vec![
            tool("1", "curl", ToolState::Completed),
            tool("2", "npm install", ToolState::Running),
        ];
        let out = compose_display(&tools, "", true, ToolDisplay::Compact);
        assert!(out.contains("✅ 1"), "expected completed count: {out}");
        assert!(out.contains("🔧 1"), "expected running count: {out}");
    }

    #[test]
    fn compose_display_none_hides_tools() {
        let tools = vec![tool(
            "1",
            "curl -s https://example.com",
            ToolState::Completed,
        )];
        let out = compose_display(&tools, "response text", false, ToolDisplay::None);
        assert_eq!(out, "response text");
    }

    #[test]
    fn contains_bot_mention_user() {
        assert!(contains_bot_mention("hello <@1234567890> world"));
    }

    #[test]
    fn contains_bot_mention_nickname() {
        assert!(contains_bot_mention("hey <@!9876543210>"));
    }

    #[test]
    fn contains_bot_mention_role() {
        assert!(contains_bot_mention("calling <@&1496247626675257384>"));
    }

    #[test]
    fn contains_bot_mention_no_match() {
        assert!(!contains_bot_mention("hello world"));
        assert!(!contains_bot_mention("email user@example.com"));
        assert!(!contains_bot_mention("<@not_a_number>"));
        assert!(!contains_bot_mention("<#123456>")); // channel mention
    }

    #[test]
    fn contains_bot_mention_embedded() {
        assert!(contains_bot_mention("請問 <@1501788608439386172> 1+1=?"));
    }
}

#[cfg(test)]
mod directive_tests {
    use super::parse_output_directives;
    use super::{classify_empty_turn, SILENT_FAILURE_MSG};
    use crate::acp::TurnResult;

    #[test]
    fn parse_reply_to_directive() {
        let input = "[[reply_to:1502606076451885136]]\nHello world";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("1502606076451885136".to_string()));
        assert_eq!(content, "Hello world");
    }

    #[test]
    fn parse_no_directives() {
        let input = "Just plain content\nwith multiple lines";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, None);
        assert_eq!(content, input);
    }

    #[test]
    fn parse_multiple_directives() {
        let input = "[[reply_to:123456]]\n[[unknown_key:value]]\nContent here";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("123456".to_string()));
        assert_eq!(content, "Content here");
    }

    #[test]
    fn parse_invalid_reply_to_rejects_whitespace() {
        let input = "[[reply_to:has spaces]]\nContent";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, None);
        assert_eq!(content, "Content");
    }

    #[test]
    fn parse_slack_ts_format_accepted() {
        let input = "[[reply_to:1234567890.123456]]\nContent";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("1234567890.123456".to_string()));
        assert_eq!(content, "Content");
    }

    #[test]
    fn parse_empty_reply_to() {
        let input = "[[reply_to:]]\nContent";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, None);
        assert_eq!(content, "Content");
    }

    #[test]
    fn parse_crlf_line_endings() {
        let input = "[[reply_to:999]]\r\nContent with CRLF";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("999".to_string()));
        assert_eq!(content, "Content with CRLF");
    }

    #[test]
    fn parse_directive_only_no_content() {
        let input = "[[reply_to:123]]";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("123".to_string()));
        assert_eq!(content, "");
    }

    #[test]
    fn parse_non_directive_line_stops_parsing() {
        let input = "Normal first line\n[[reply_to:123]]\nMore content";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, None);
        assert_eq!(content, input);
    }

    #[test]
    fn parse_duplicate_reply_to_last_wins() {
        let input = "[[reply_to:111]]\n[[reply_to:222]]\nContent";
        let (directives, content) = parse_output_directives(input);
        // Last value wins
        assert_eq!(directives.reply_to, Some("222".to_string()));
        assert_eq!(content, "Content");
    }

    #[test]
    fn parse_crlf_multiple_directives() {
        let input = "[[reply_to:456]]\r\n[[unknown:x]]\r\nContent after CRLF";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("456".to_string()));
        assert_eq!(content, "Content after CRLF");
    }

    #[test]
    fn parse_bracket_without_colon_preserved() {
        // [[Note]] has no colon — not a directive, preserved as content
        let input = "[[Summary]]\nThis is body text";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, None);
        assert_eq!(content, input);
    }

    #[test]
    fn parse_reply_to_with_inline_content() {
        // Agent puts content on same line as directive — should still parse
        let input = "[[reply_to:1502724086474870926]]  @BOT I'm on standby";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("1502724086474870926".to_string()));
        assert_eq!(content, "@BOT I'm on standby");
    }

    #[test]
    fn parse_reply_to_inline_with_more_lines() {
        let input = "[[reply_to:123]]  First line\nSecond line\nThird line";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("123".to_string()));
        assert_eq!(content, "First line\nSecond line\nThird line");
    }

    #[test]
    fn parse_reply_to_no_space_before_content() {
        // No space between ]] and content
        let input = "[[reply_to:1502724086474870926]]收到";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("1502724086474870926".to_string()));
        assert_eq!(content, "收到");
    }

    #[test]
    fn parse_reply_to_inline_with_mention() {
        // Real-world case: directive followed by Discord mention
        let input = "[[reply_to:1502724086474870926]]  <@1490365068863606784> 我 standby";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("1502724086474870926".to_string()));
        assert_eq!(content, "<@1490365068863606784> 我 standby");
    }

    #[test]
    fn parse_reply_to_inline_only_spaces() {
        // Trailing spaces only — no real content, should be empty
        let input = "[[reply_to:123]]   ";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("123".to_string()));
        assert_eq!(content, "");
    }

    #[test]
    fn parse_reply_to_with_brackets_in_content() {
        // Content after ]] contains brackets — should not confuse parser
        let input = "[[reply_to:456]]  看看 [[這個]] 怎麼樣";
        let (directives, content) = parse_output_directives(input);
        assert_eq!(directives.reply_to, Some("456".to_string()));
        assert_eq!(content, "看看 [[這個]] 怎麼樣");
    }

    // --- classify_empty_turn: adapter-level finalization tests ---

    #[test]
    fn empty_turn_silent_failure_produces_diagnostic() {
        let tr = TurnResult {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(0),
            input_tokens: Some(0),
            total_tokens: Some(0),
        };
        let result = classify_empty_turn(None, &tr);
        assert_eq!(result, SILENT_FAILURE_MSG);
    }

    #[test]
    fn empty_turn_silent_failure_nonzero_input_still_diagnostic() {
        let tr = TurnResult {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(0),
            input_tokens: Some(150),
            total_tokens: Some(150),
        };
        let result = classify_empty_turn(None, &tr);
        assert_eq!(result, SILENT_FAILURE_MSG);
    }

    #[test]
    fn empty_turn_response_error_takes_precedence() {
        let tr = TurnResult {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(0),
            input_tokens: Some(0),
            total_tokens: Some(0),
        };
        let result = classify_empty_turn(Some("Agent process died"), &tr);
        assert_eq!(result, "⚠️ Agent process died");
    }

    #[test]
    fn empty_turn_missing_usage_shows_no_response() {
        let tr = TurnResult::default();
        let result = classify_empty_turn(None, &tr);
        assert_eq!(result, "_(no response)_");
    }

    #[test]
    fn empty_turn_nonzero_output_shows_no_response() {
        let tr = TurnResult {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(50),
            input_tokens: Some(10),
            total_tokens: Some(60),
        };
        let result = classify_empty_turn(None, &tr);
        assert_eq!(result, "_(no response)_");
    }

    #[test]
    fn empty_turn_different_stop_reason_shows_no_response() {
        let tr = TurnResult {
            stop_reason: Some("max_tokens".into()),
            output_tokens: Some(0),
            input_tokens: Some(10),
            total_tokens: Some(10),
        };
        let result = classify_empty_turn(None, &tr);
        assert_eq!(result, "_(no response)_");
    }
}

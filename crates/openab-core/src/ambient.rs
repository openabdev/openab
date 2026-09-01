//! Ambient Mode — batch flush dispatcher for passive channel listening.
//!
//! See ADR: docs/adr/ambient.md for full design rationale.
//!
//! Ambient mode listens to all messages in configured channels (without @mention)
//! and periodically flushes them as a batch to the LLM. The agent replies only
//! when it has something valuable to add; otherwise it returns `[NO_REPLY]`.
//!
//! # Why a standalone module (not extending Dispatcher)
//!
//! ADR §Implementation Notes suggests reusing Dispatcher infrastructure. This
//! implementation deliberately builds a separate module because ambient's
//! requirements diverge from normal dispatch:
//!
//! - No trigger message — passive listening has no `MessageRef` to anchor
//!   reactions or reply_to.
//! - No streaming / placeholder — ambient is non-interactive; responses use
//!   `AmbientCaptureAdapter` (non-streaming) for `[NO_REPLY]` pre-filtering.
//! - No per-sender batching (Lane mode) — ambient batches by channel, not sender.
//! - No `BotTurnTracker` integration — ambient has independent reply budget (v2).
//!
//! Forcing these into `Dispatcher` would require pervasive `if ambient { ... }`
//! branches, increasing regression risk for the existing dispatch path. Clean
//! separation keeps both paths simple and independently testable.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, info, warn};

use crate::acp::ContentBlock;
use crate::adapter::{ChannelRef, ChatAdapter, MessageRef};
use crate::config::AmbientConfig;
use crate::dispatch::DispatchTarget;

use anyhow::Result;
use async_trait::async_trait;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sentinel value the agent returns when it has nothing to add.
const NO_REPLY_SENTINEL: &str = "[no_reply]";

/// Maximum characters to read from the instructions file.
const INSTRUCTIONS_FILE_MAX_CHARS: usize = 2000;

/// Default system instruction used when no instructions file is found.
const DEFAULT_AMBIENT_SYSTEM_INSTRUCTION: &str = r#"You are in ambient mode. Below is a batch of recent messages from the channel.

Rules:
- Reply when someone asks a question, needs help, or when you can add useful context
- If the conversation is purely social chatter with no question or actionable topic, reply EXACTLY: [NO_REPLY]
- When replying, always @mention the person you are responding to (e.g. <@123456789>) so they get notified. If multiple people are relevant, mention all of them.
- Keep replies concise and natural — you are joining an ongoing conversation, not starting one
- Do not acknowledge that you are in ambient mode
"#;

/// Load ambient system instruction from the configured file path.
/// Falls back to `DEFAULT_AMBIENT_SYSTEM_INSTRUCTION` if the file does not exist.
/// Truncates to `INSTRUCTIONS_FILE_MAX_CHARS` characters.
fn load_instructions(path: &str) -> String {
    let expanded = if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(stripped)
        } else {
            std::path::PathBuf::from(path)
        }
    } else {
        std::path::PathBuf::from(path)
    };

    // Warn if path is outside $HOME
    if let Some(home) = std::env::var_os("HOME") {
        if !expanded.starts_with(std::path::Path::new(&home)) {
            warn!(path = %expanded.display(), "ambient: instructions_file is outside $HOME");
        }
    }

    match std::fs::read_to_string(&expanded) {
        Ok(content) => {
            let char_count = content.chars().count();
            let truncated: String = content.chars().take(INSTRUCTIONS_FILE_MAX_CHARS).collect();
            if char_count > INSTRUCTIONS_FILE_MAX_CHARS {
                warn!(
                    path = %expanded.display(),
                    original_chars = char_count,
                    max = INSTRUCTIONS_FILE_MAX_CHARS,
                    "ambient: instructions file truncated"
                );
            }
            info!(path = %expanded.display(), chars = truncated.len(), "ambient: loaded custom instructions");
            truncated
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %expanded.display(), "ambient: instructions file not found, using default");
            DEFAULT_AMBIENT_SYSTEM_INSTRUCTION.to_string()
        }
        Err(e) => {
            warn!(path = %expanded.display(), error = %e, "ambient: failed to read instructions file, using default");
            DEFAULT_AMBIENT_SYSTEM_INSTRUCTION.to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// AmbientMessage — lighter than BufferedMessage for ambient buffering
// ---------------------------------------------------------------------------

/// A single message buffered for ambient dispatch.
#[derive(Debug)]
pub struct AmbientMessage {
    /// Author display name.
    pub sender_name: String,
    /// Discord UID of the sender (for @mentions in replies).
    pub sender_id: String,
    /// User-visible prompt text.
    pub prompt: String,
    /// Attachment blocks.
    pub extra_blocks: Vec<ContentBlock>,
    /// When this message arrived.
    pub arrived_at: Instant,
}

// ---------------------------------------------------------------------------
// FlushingGuard — RAII guard for the per-channel flushing flag with timeout
// ---------------------------------------------------------------------------

/// RAII guard that sets an `AtomicBool` to true on creation and resets to false
/// on drop. Also spawns a safety timeout that forcibly resets if the guard is
/// held too long (e.g., consumer panic under a catch_unwind or OOM).
struct FlushingGuard {
    flag: Arc<AtomicBool>,
    _timeout_handle: tokio::task::JoinHandle<()>,
}

impl FlushingGuard {
    fn new(flag: Arc<AtomicBool>, timeout: Duration) -> Self {
        flag.store(true, Ordering::Release);
        let flag_clone = Arc::clone(&flag);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if flag_clone.swap(false, Ordering::AcqRel) {
                warn!("ambient flush timeout exceeded, force-resetting flushing flag");
            }
        });
        Self {
            flag,
            _timeout_handle: handle,
        }
    }
}

impl Drop for FlushingGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
        self._timeout_handle.abort();
    }
}

// ---------------------------------------------------------------------------
// PostGuard — atomic check-and-post to prevent TOCTOU race
// ---------------------------------------------------------------------------

/// Per-channel post guard. The ambient consumer acquires it before posting;
/// the mention path invalidates it to cancel an in-flight ambient response.
#[derive(Debug)]
pub struct PostGuard {
    cancelled: AtomicBool,
}

impl Default for PostGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl PostGuard {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    /// Cancel any pending ambient post for this channel.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check if posting is still allowed. Returns false if cancelled.
    pub fn can_post(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
    }

    /// Reset for next flush cycle.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// AmbientCaptureAdapter — intercepts send_message for [NO_REPLY] filtering
// ---------------------------------------------------------------------------

/// Wraps a real `ChatAdapter` and intercepts `send_message` to suppress
/// `[NO_REPLY]` responses before they reach the channel.
struct AmbientCaptureAdapter {
    inner: Arc<dyn ChatAdapter>,
    debug: bool,
}

#[async_trait]
impl ChatAdapter for AmbientCaptureAdapter {
    fn platform(&self) -> &'static str {
        self.inner.platform()
    }

    fn message_limit(&self) -> usize {
        self.inner.message_limit()
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false // Force non-streaming so text is collected before send
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        // Filter [NO_REPLY] before it reaches the channel (unless debug mode).
        if is_no_reply(content) && !self.debug {
            debug!("ambient: suppressed [NO_REPLY] response");
            return Ok(MessageRef {
                channel: channel.clone(),
                message_id: String::new(),
            });
        }
        self.inner.send_message(channel, content).await
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        self.inner.create_thread(channel, trigger_msg, title).await
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        self.inner.add_reaction(msg, emoji).await
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        self.inner.remove_reaction(msg, emoji).await
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        self.inner.edit_message(msg, content).await
    }

    async fn send_message_with_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        reply_to_message_id: &str,
    ) -> Result<MessageRef> {
        if is_no_reply(content) && !self.debug {
            debug!("ambient: suppressed [NO_REPLY] reply");
            return Ok(MessageRef {
                channel: channel.clone(),
                message_id: String::new(),
            });
        }
        self.inner
            .send_message_with_reply(channel, content, reply_to_message_id)
            .await
    }

    async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
        self.inner.delete_message(msg).await
    }
}

// ---------------------------------------------------------------------------
// ChannelState — per-channel ambient state
// ---------------------------------------------------------------------------

struct ChannelState {
    tx: tokio::sync::mpsc::Sender<AmbientMessage>,
    flushing: Arc<AtomicBool>,
    post_guard: Arc<PostGuard>,
    _consumer: tokio::task::JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// AmbientDispatcher
// ---------------------------------------------------------------------------

/// Manages ambient mode across all configured channels.
///
/// Each enabled channel gets its own mpsc channel and consumer task.
/// The consumer accumulates messages and flushes them as a batch when
/// a time or count trigger fires.
pub struct AmbientDispatcher {
    config: AmbientConfig,
    channels: Mutex<HashMap<String, ChannelState>>,
    /// Channel IDs that have ambient mode enabled (pre-parsed for fast lookup).
    enabled_channels: HashSet<u64>,
    /// Global semaphore limiting concurrent flush operations.
    flush_semaphore: Arc<Semaphore>,
    /// Loaded system instruction (from file or default).
    instructions: String,
}

impl AmbientDispatcher {
    /// Create a new AmbientDispatcher from config.
    ///
    /// Does NOT start consumer tasks — those are spawned lazily on first message
    /// or eagerly via `start_channel`.
    pub fn new(config: AmbientConfig) -> Self {
        let enabled_channels: HashSet<u64> = config
            .discord
            .channels
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        let flush_semaphore = Arc::new(Semaphore::new(config.max_concurrent_flushes.max(1)));
        let instructions = load_instructions(&config.instructions_file);
        if config.enabled && !enabled_channels.is_empty() {
            tracing::info!(
                channels = ?enabled_channels,
                "ambient: thread observation is default-on for configured channels"
            );
        }
        Self {
            config,
            channels: Mutex::new(HashMap::new()),
            enabled_channels,
            flush_semaphore,
            instructions,
        }
    }

    /// Check if ambient mode is active and this channel is in the allowlist.
    pub fn is_ambient_channel(&self, channel_id: u64) -> bool {
        self.config.enabled
            && !self.enabled_channels.is_empty()
            && self.enabled_channels.contains(&channel_id)
    }

    /// Decide whether a message should be ambient-buffered.
    ///
    /// - Top-level message directly in an ambient channel → yes (buffer keyed by
    ///   `channel_id`).
    /// - Thread message → yes when the thread's `parent_id` is an ambient channel,
    ///   regardless of whether the bot owns the thread. Bot-owned threads are
    ///   also observed so the bot can passively follow conversation without
    ///   requiring an @mention. An @mention in any ambient context discards the
    ///   buffer and falls through to immediate dispatch — no double handling.
    ///   Threads are batched independently (keyed by the thread's `channel_id`).
    ///
    /// Threads under an ambient channel are observed by default — most OpenAB
    /// conversation happens in auto-created threads, not the parent channel.
    ///
    /// Returns false when ambient is disabled or no channels are configured.
    pub fn should_buffer(
        &self,
        channel_id: u64,
        in_thread: bool,
        _bot_owns_thread: bool,
        parent_id: Option<u64>,
    ) -> bool {
        if !self.config.enabled || self.enabled_channels.is_empty() {
            return false;
        }
        if !in_thread {
            return self.enabled_channels.contains(&channel_id);
        }
        parent_id.is_some_and(|p| self.enabled_channels.contains(&p))
    }

    /// Whether bot messages are allowed in the ambient buffer.
    pub fn allow_bot_messages(&self) -> bool {
        self.config.discord.allow_bot_messages
    }

    /// Submit a message to the ambient buffer for a channel.
    ///
    /// Returns Ok(()) if buffered, Err if the channel consumer is dead.
    pub async fn submit(
        &self,
        channel_id: &str,
        channel_ref: ChannelRef,
        adapter: Arc<dyn ChatAdapter>,
        target: Arc<dyn DispatchTarget>,
        msg: AmbientMessage,
    ) {
        let mut channels = self.channels.lock().await;

        // Lazily spawn consumer if not yet started for this channel.
        if !channels.contains_key(channel_id) {
            let (tx, rx) = tokio::sync::mpsc::channel(self.config.flush_hard_cap.max(1));
            let flushing = Arc::new(AtomicBool::new(false));
            let post_guard = Arc::new(PostGuard::new());

            let consumer = tokio::spawn(ambient_consumer_loop(
                channel_id.to_string(),
                channel_ref.clone(),
                rx,
                self.config.clone(),
                Arc::clone(&self.flush_semaphore),
                Arc::clone(&flushing),
                Arc::clone(&post_guard),
                adapter,
                target,
                self.instructions.clone(),
            ));

            channels.insert(
                channel_id.to_string(),
                ChannelState {
                    tx,
                    flushing,
                    post_guard,
                    _consumer: consumer,
                },
            );
        }

        let state = channels.get(channel_id).unwrap();
        // Non-blocking try_send — if buffer is full (hard_cap), drop the message.
        if let Err(e) = state.tx.try_send(msg) {
            debug!(channel_id, "ambient buffer full, dropping message: {}", e);
        }
    }

    /// Discard the ambient buffer for a channel (called when @mention arrives).
    /// Also cancels any in-flight ambient response via the post_guard.
    /// The consumer discards the current batch; remaining buffered messages
    /// carry into the next cycle.
    pub async fn discard_buffer(&self, channel_id: &str) {
        let channels = self.channels.lock().await;
        if let Some(state) = channels.get(channel_id) {
            // Cancel any in-flight post — consumer discards current batch on next check.
            state.post_guard.cancel();
            debug!(
                channel_id,
                "ambient buffer discard requested (mention arrived)"
            );
        }
    }

    /// Check if a channel is currently mid-flush.
    /// Used by v2 rate-limiting (min_flush_interval_seconds) — kept for forward compat.
    #[allow(dead_code)]
    pub async fn is_flushing(&self, channel_id: &str) -> bool {
        let channels = self.channels.lock().await;
        channels
            .get(channel_id)
            .map(|s| s.flushing.load(Ordering::Acquire))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// ambient_consumer_loop
// ---------------------------------------------------------------------------

/// Per-channel consumer that accumulates messages and flushes them as a batch.
#[allow(clippy::too_many_arguments)]
async fn ambient_consumer_loop(
    channel_id: String,
    channel_ref: ChannelRef,
    mut rx: tokio::sync::mpsc::Receiver<AmbientMessage>,
    config: AmbientConfig,
    flush_semaphore: Arc<Semaphore>,
    flushing: Arc<AtomicBool>,
    post_guard: Arc<PostGuard>,
    adapter: Arc<dyn ChatAdapter>,
    target: Arc<dyn DispatchTarget>,
    instructions: String,
) {
    info!(channel_id = %crate::redact::redact_session_ids(&channel_id), "ambient consumer started");

    loop {
        // Wait for first message (blocks until one arrives or channel closes).
        let first = match rx.recv().await {
            Some(msg) => msg,
            None => {
                info!(channel_id = %crate::redact::redact_session_ids(&channel_id), "ambient consumer channel closed, exiting");
                return;
            }
        };

        // Reset post_guard at the start of each batch cycle. This ensures a
        // previous cancellation doesn't permanently block future cycles.
        post_guard.reset();

        // Compute jittered deadline: flush_interval ± 20%
        // Guard: interval must be >= 1s to avoid gen_range panic on empty range.
        let base_secs = config.flush_interval_seconds.max(1);
        let base = Duration::from_secs(base_secs);
        let jitter_range = base.as_millis() as f64 * 0.2;
        let jitter_ms = rand::thread_rng().gen_range(-jitter_range..jitter_range) as i64;
        let interval =
            Duration::from_millis((base.as_millis() as i64 + jitter_ms).max(1000) as u64);
        let deadline = tokio::time::Instant::now() + interval;

        let mut batch = vec![first];
        // Guard: flush_max_messages must be >= 1 to avoid immediate single-msg flush.
        let flush_max = config.flush_max_messages.max(1);

        // Accumulate until trigger fires.
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break; // Timer expired.
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(msg)) => {
                    batch.push(msg);
                    if batch.len() >= flush_max {
                        break;
                    }
                    if batch.len() >= config.flush_hard_cap.max(1) {
                        break;
                    }
                }
                Ok(None) => break, // Channel closed.
                Err(_) => break,   // Timer expired.
            }
        }

        let batch_size = batch.len();
        debug!(
            channel_id = %crate::redact::redact_session_ids(&channel_id),
            batch_size,
            "ambient flush triggered"
        );

        // Acquire global concurrency permit.
        let _permit = match flush_semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                warn!(channel_id = %crate::redact::redact_session_ids(&channel_id), "flush semaphore closed, exiting");
                return;
            }
        };

        // Set flushing flag with safety timeout (clamped to [5s, 600s]).
        let flush_timeout = Duration::from_secs(config.flush_timeout_seconds.clamp(5, 600));
        let _flushing_guard = FlushingGuard::new(Arc::clone(&flushing), flush_timeout);

        // Check post_guard BEFORE building payload (mention may have cancelled during accumulation).
        if !post_guard.can_post() {
            // Don't drain — messages buffered after the mention are still valid
            // for the next batch cycle. The current batch is discarded but future
            // messages will be picked up when the loop restarts and reset() clears.
            debug!(channel_id = %crate::redact::redact_session_ids(&channel_id), "ambient flush cancelled by mention during accumulation");
            continue;
        }

        // Build the batch payload.
        let session_key = format!("ambient:{}:{}", channel_ref.platform, channel_id);
        let content_blocks = build_ambient_payload(&batch, &instructions);

        // Debug mode: notify channel that a flush is happening.
        if config.debug {
            let senders: Vec<&str> = batch.iter().map(|m| m.sender_name.as_str()).collect();
            let transcript = batch
                .iter()
                .map(|m| {
                    format!(
                        "{} (<@{}>): {}",
                        m.sender_name,
                        m.sender_id,
                        m.prompt.replace('`', "'")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut debug_msg = format!(
                "🔍 **ambient debug** — flushing {} message(s) from: {}\n**System prompt:**\n```\n{}\n```\n**Transcript:**\n```\n{}\n```",
                batch_size,
                senders.join(", "),
                instructions.replace('`', "'"),
                transcript
            );
            // Truncate to stay within Discord's 2000-char message limit (UTF-8 safe).
            if debug_msg.len() > 1900 {
                let mut end = 1900;
                while !debug_msg.is_char_boundary(end) {
                    end -= 1;
                }
                debug_msg.truncate(end);
                debug_msg.push_str("\n…(truncated)");
            }
            if let Err(e) = adapter.send_message(&channel_ref, &debug_msg).await {
                warn!(channel_id = %crate::redact::redact_session_ids(&channel_id), error = %e, "ambient debug message send failed");
            }
        }

        // Ensure session exists.
        if let Err(e) = target.ensure_session(&session_key, None, None).await {
            warn!(
                channel_id = %crate::redact::redact_session_ids(&channel_id),
                error = %e,
                "failed to create ambient session, discarding batch"
            );
            continue;
        }

        // Dispatch batch to agent using AmbientCaptureAdapter which intercepts
        // [NO_REPLY] responses before they reach the channel. Non-streaming mode
        // ensures text is fully collected before send_message is called, allowing
        // is_no_reply() to filter the sentinel pre-delivery.
        //
        // ⚠️ KNOWN LIMITATION (v1, accepted):
        // Tool access: ambient flush shares the same DispatchTarget as @mention.
        // v2 should use a restricted target or disable tools for ambient sessions.
        let capture_adapter: Arc<dyn ChatAdapter> = Arc::new(AmbientCaptureAdapter {
            inner: Arc::clone(&adapter),
            debug: config.debug,
        });
        let dummy_msg_ref = MessageRef {
            channel: channel_ref.clone(),
            message_id: String::new(),
        };
        let reactions = Arc::new(crate::reactions::StatusReactionController::new(
            false, // disabled — no reactions for ambient
            Arc::clone(&capture_adapter),
            dummy_msg_ref,
            crate::config::ReactionEmojis::default(),
            crate::config::ReactionTiming::default(),
        ));

        // Check post_guard before dispatching (mention may have cancelled).
        if !post_guard.can_post() {
            debug!(channel_id = %crate::redact::redact_session_ids(&channel_id), "ambient flush cancelled by mention before dispatch");
            continue;
        }

        match target
            .stream_prompt_blocks(
                &capture_adapter,
                &session_key,
                content_blocks,
                &channel_ref,
                reactions,
                false, // other_bot_present
                None,  // no streaming recipient
            )
            .await
        {
            Ok(((), _)) => {
                debug!(channel_id = %crate::redact::redact_session_ids(&channel_id), "ambient flush dispatched");
            }
            Err(e) => {
                warn!(
                    channel_id = %crate::redact::redact_session_ids(&channel_id),
                    error = %e,
                    "ambient flush failed, discarding batch"
                );
            }
        }

        // _flushing_guard drops here → resets flag
        // _permit drops here → releases semaphore
    }
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

/// Build the content blocks for an ambient batch dispatch.
fn build_ambient_payload(batch: &[AmbientMessage], instructions: &str) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    // System instruction from file.
    blocks.push(ContentBlock::Text {
        text: instructions.to_string(),
    });

    // Format batch as conversation transcript.
    let mut transcript = String::from("[Ambient batch — ");
    transcript.push_str(&batch.len().to_string());
    transcript.push_str(" new messages]\n");

    for msg in batch {
        // NOTE: Discord-specific mention format (<@UID>). When adding other
        // platforms, pass a formatter or adapt per-platform. (v2)
        transcript.push_str(&msg.sender_name);
        transcript.push_str(" (<@");
        transcript.push_str(&msg.sender_id);
        transcript.push_str(">): ");
        transcript.push_str(&msg.prompt);
        transcript.push('\n');
    }

    transcript.push_str("\n[End of batch — reply only if you can add meaningful value.\n Otherwise reply exactly: [NO_REPLY]]");

    blocks.push(ContentBlock::Text { text: transcript });

    // Append any attachment blocks from messages.
    for msg in batch {
        blocks.extend(msg.extra_blocks.iter().cloned());
    }

    blocks
}

/// Check if a response is the NO_REPLY sentinel.
pub fn is_no_reply(response: &str) -> bool {
    response.trim().to_lowercase() == NO_REPLY_SENTINEL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_text(blocks: &[ContentBlock]) -> &str {
        match &blocks[0] {
            ContentBlock::Text { text } => text.as_str(),
            #[allow(unreachable_patterns)]
            _ => panic!("first block is not text"),
        }
    }

    fn msg(s: &str) -> AmbientMessage {
        AmbientMessage {
            sender_name: "Pahud".into(),
            sender_id: "845835116920307722".into(),
            prompt: s.into(),
            extra_blocks: Vec::new(),
            arrived_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn payload_without_instructions_is_default_prompt() {
        let blocks = build_ambient_payload(&[msg("hi")], super::DEFAULT_AMBIENT_SYSTEM_INSTRUCTION);
        let sys = first_text(&blocks);
        assert!(sys.contains("You are in ambient mode"));
        assert!(sys.contains("[NO_REPLY]"));
        assert!(sys.contains("@mention the person"));
    }

    #[test]
    fn payload_uses_custom_instructions() {
        let blocks = build_ambient_payload(&[msg("hi")], "Custom prompt with [NO_REPLY] rule.");
        let sys = first_text(&blocks);
        assert!(sys.contains("Custom prompt"));
        assert!(sys.contains("[NO_REPLY]"));
    }

    #[test]
    fn payload_transcript_includes_sender_uid() {
        let blocks =
            build_ambient_payload(&[msg("hello")], super::DEFAULT_AMBIENT_SYSTEM_INSTRUCTION);
        let transcript = match &blocks[1] {
            ContentBlock::Text { text } => text.as_str(),
            #[allow(unreachable_patterns)]
            _ => panic!("second block is not text"),
        };
        assert!(transcript.contains("Pahud (<@845835116920307722>): hello"));
    }

    fn dispatcher(channels: &[&str], enabled: bool) -> AmbientDispatcher {
        let config = AmbientConfig {
            enabled,
            discord: crate::config::AmbientDiscordConfig {
                channels: channels.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        AmbientDispatcher::new(config)
    }

    #[test]
    fn should_buffer_toplevel_message_in_ambient_channel() {
        let d = dispatcher(&["100"], true);
        assert!(d.should_buffer(100, false, false, None));
        // Different channel → not buffered.
        assert!(!d.should_buffer(200, false, false, None));
    }

    #[test]
    fn should_buffer_thread_under_ambient_channel_by_default() {
        let d = dispatcher(&["100"], true);
        // Thread under an ambient channel, bot does not own it → buffer.
        assert!(d.should_buffer(999, true, false, Some(100)));
        // Thread whose parent is NOT an ambient channel → no.
        assert!(!d.should_buffer(999, true, false, Some(200)));
        // Thread the bot owns → ALSO buffered (bot passively observes its own
        // threads; @mention triggers immediate dispatch with buffer discard).
        assert!(d.should_buffer(999, true, true, Some(100)));
        // Thread with no resolvable parent → no.
        assert!(!d.should_buffer(999, true, false, None));
    }

    #[test]
    fn should_buffer_false_when_ambient_disabled() {
        let d = dispatcher(&["100"], false);
        assert!(!d.should_buffer(100, false, false, None));
        assert!(!d.should_buffer(999, true, false, Some(100)));
    }

    #[test]
    fn is_no_reply_exact() {
        assert!(is_no_reply("[NO_REPLY]"));
        assert!(is_no_reply("[no_reply]"));
        assert!(is_no_reply("[No_Reply]"));
    }

    #[test]
    fn is_no_reply_with_whitespace() {
        assert!(is_no_reply("  [NO_REPLY]  "));
        assert!(is_no_reply("\n[no_reply]\n"));
        assert!(is_no_reply("\t [NO_REPLY] \t"));
    }

    #[test]
    fn is_no_reply_rejects_partial() {
        assert!(!is_no_reply("NO_REPLY"));
        assert!(!is_no_reply("[NO_REPLY] sure"));
        assert!(!is_no_reply("I have [NO_REPLY] for you"));
        assert!(!is_no_reply(""));
    }

    #[test]
    fn post_guard_lifecycle() {
        let guard = PostGuard::new();
        assert!(guard.can_post());

        guard.cancel();
        assert!(!guard.can_post());

        guard.reset();
        assert!(guard.can_post());
    }

    #[test]
    fn post_guard_double_cancel() {
        let guard = PostGuard::new();
        guard.cancel();
        guard.cancel();
        assert!(!guard.can_post());

        guard.reset();
        assert!(guard.can_post());
    }

    #[test]
    fn load_instructions_file_exists() {
        let dir = std::env::temp_dir().join("oab_test_ambient");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_instructions.md");
        std::fs::write(&path, "custom prompt").unwrap();

        let result = super::load_instructions(path.to_str().unwrap());
        assert_eq!(result, "custom prompt");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_instructions_file_missing_fallback() {
        let result = super::load_instructions("/tmp/nonexistent_oab_test_file_xyz.md");
        assert_eq!(result, super::DEFAULT_AMBIENT_SYSTEM_INSTRUCTION);
    }

    #[test]
    fn load_instructions_truncates_at_limit() {
        let dir = std::env::temp_dir().join("oab_test_ambient_trunc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long_instructions.md");
        let long_content = "x".repeat(3000);
        std::fs::write(&path, &long_content).unwrap();

        let result = super::load_instructions(path.to_str().unwrap());
        assert_eq!(result.len(), super::INSTRUCTIONS_FILE_MAX_CHARS);

        std::fs::remove_dir_all(&dir).ok();
    }
}

//! Turn-boundary message batching dispatcher.
//!
//! See ADR: docs/adr/turn-boundary-batching.md for full design rationale.
//!
//! # Invariants
//! - I1: First message after idle has zero added latency.
//! - I2: At most one in-flight ACP turn per thread.
//! - I3: Broker structural fidelity — no merging, splitting, reordering, or
//!   semantic transformation of arrival events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, error, info, info_span, warn};

use crate::acp::ContentBlock;
use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use crate::config::ReactionsConfig;
use crate::error_display::format_user_error;
use crate::reactions::StatusReactionController;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One arrival event buffered for a future ACP turn.
pub struct BufferedMessage {
    /// Serialised SenderContext JSON (already built by the platform adapter).
    pub sender_json: String,
    /// Author display name — denormalised from `sender_json` so observability
    /// fields (per-event tracing in `dispatch_batch`) don't pay a JSON parse.
    /// Per ADR §2.3 each arrival event carries its sender name.
    pub sender_name: String,
    /// User-visible prompt text (verbatim, never transformed).
    pub prompt: String,
    /// Attachment blocks (images, STT transcripts) in arrival order.
    pub extra_blocks: Vec<ContentBlock>,
    /// Anchor for reactions (👀 / ❌).
    pub trigger_msg: MessageRef,
    /// Broker receive time — used for `buffer_wait_ms` observability.
    pub arrived_at: Instant,
    /// Rough token estimate for `max_batch_tokens` cap.
    pub estimated_tokens: usize,
    /// Snapshot at submit time. Captured per-message so a batch reflects the
    /// freshest known state; `dispatch_batch` reads `batch.last()`.
    pub other_bot_present: bool,
}

/// How `thread_key` is built for the dispatcher's per-thread map.
///
/// - `Thread`: one mpsc per thread → all senders in a thread share one batch → one
///   ACP turn per batch (cheaper, but risks silent drop when the agent's single reply
///   forgets to address some senders).
/// - `Lane`: one mpsc per (thread, sender) → each sender batches independently and
///   gets a dedicated ACP turn. Sessions are still shared per-thread; turns serialise
///   through the shared session.
///
/// Derived from `config::MessageProcessingMode` in `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchGrouping {
    Thread,
    Lane,
}

/// Error returned by `Dispatcher::submit`.
#[derive(Debug)]
pub enum DispatchError {
    /// The per-thread consumer task has exited unexpectedly.
    ConsumerDead,
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConsumerDead => write!(f, "dispatch consumer exited unexpectedly"),
        }
    }
}

impl std::error::Error for DispatchError {}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct ThreadHandle {
    tx: tokio::sync::mpsc::Sender<BufferedMessage>,
    consumer: tokio::task::JoinHandle<()>,
    /// Race-safe eviction counter (§2.5). Plain u64 — all reads/writes under per_thread lock.
    generation: u64,
    channel_id: String,
    adapter_kind: String,
}

impl ThreadHandle {
    /// Approximate number of messages still buffered in the mpsc — used for
    /// shutdown / cancel logging. Not exact: tokio's mpsc has no sync `.len()`.
    fn pending_count(&self) -> usize {
        self.tx.max_capacity() - self.tx.capacity()
    }
}

// ---------------------------------------------------------------------------
// DispatchTarget — trait seam between Dispatcher and AdapterRouter
// ---------------------------------------------------------------------------

/// Surface that `consumer_loop` / `dispatch_batch` need from the underlying
/// router. Extracted as a trait so the dispatcher can be unit-tested without
/// spinning up a real `SessionPool` (which forks ACP CLI subprocesses).
/// `AdapterRouter` is the production implementor; tests use a mock that
/// records calls.
#[async_trait]
pub trait DispatchTarget: Send + Sync + 'static {
    fn reactions_config(&self) -> &ReactionsConfig;

    /// Ensure the ACP session for `session_key` exists (idempotent).
    async fn ensure_session(&self, session_key: &str) -> Result<()>;

    /// Drive one ACP turn with the pre-packed `content_blocks`.
    #[allow(clippy::too_many_arguments)]
    async fn stream_prompt_blocks(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        session_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
        other_bot_present: bool,
    ) -> Result<()>;
}

#[async_trait]
impl DispatchTarget for AdapterRouter {
    fn reactions_config(&self) -> &ReactionsConfig {
        AdapterRouter::reactions_config(self)
    }

    async fn ensure_session(&self, session_key: &str) -> Result<()> {
        self.pool().get_or_create(session_key).await
    }

    async fn stream_prompt_blocks(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        session_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
        other_bot_present: bool,
    ) -> Result<()> {
        AdapterRouter::stream_prompt_blocks(
            self,
            adapter,
            session_key,
            content_blocks,
            thread_channel,
            reactions,
            other_bot_present,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Default idle timeout for per-thread consumer tasks in batched modes (Thread / Lane).
/// When no message arrives within this window the consumer exits, allowing `per_thread`
/// map cleanup on the next `submit` (via `SendError` → `try_evict_locked`). Prevents
/// unbounded task/memory growth from one-shot thread keys (e.g. Slack non-thread messages).
///
/// Batched modes need a longer window so a lane that's between trigger arrivals isn't
/// torn down and respawned on every message.
pub const DEFAULT_CONSUMER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Idle timeout for per-message mode (cap=1, no batching). Per-message dispatchers
/// don't benefit from holding consumers across message gaps — there is no batch
/// window to preserve — so a much shorter timeout reduces idle resource footprint
/// from one-shot thread keys (Little's Law: steady-state idle count = arrival rate
/// × idle window).
pub const PER_MESSAGE_CONSUMER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve `(cap, grouping, idle_timeout)` for a given processing mode.
///
/// Per-message mode forces cap=1 + Thread grouping + the short per-message idle
/// (one-shot threads shouldn't pin a consumer for 5 min); Thread / Lane modes
/// use the configured `max_buffered` and the default idle window.
pub fn dispatch_params(
    mode: &crate::config::MessageProcessingMode,
    max_buffered: usize,
) -> (usize, BatchGrouping, Duration) {
    use crate::config::MessageProcessingMode;
    match mode {
        MessageProcessingMode::Message => {
            (1, BatchGrouping::Thread, PER_MESSAGE_CONSUMER_IDLE_TIMEOUT)
        }
        MessageProcessingMode::Thread => (
            max_buffered,
            BatchGrouping::Thread,
            DEFAULT_CONSUMER_IDLE_TIMEOUT,
        ),
        MessageProcessingMode::Lane => (
            max_buffered,
            BatchGrouping::Lane,
            DEFAULT_CONSUMER_IDLE_TIMEOUT,
        ),
    }
}

/// Per-thread message dispatcher for batched mode.
///
/// Constructed once in `main.rs` and shared via `Arc`. Platform adapters call
/// `submit()` from their per-message `tokio::spawn`'d tasks.
pub struct Dispatcher {
    /// std::sync::Mutex — critical section has no .await; tokio::Mutex buys nothing here.
    per_thread: Mutex<HashMap<String, ThreadHandle>>,
    /// Monotonic counter for `ThreadHandle.generation` (§2.5). Pre-fetched on
    /// every `submit` and consumed only when a fresh handle is inserted; wasted
    /// values are fine because generations need only be monotonic, not contiguous.
    next_generation: AtomicU64,
    target: Arc<dyn DispatchTarget>,
    max_buffered_messages: usize,
    max_batch_tokens: usize,
    grouping: BatchGrouping,
    idle_timeout: Duration,
}

impl Dispatcher {
    /// Construct a dispatcher with an explicit consumer idle timeout. Per-mode
    /// callers in `main.rs` pass `PER_MESSAGE_CONSUMER_IDLE_TIMEOUT` for cap=1
    /// dispatchers and `DEFAULT_CONSUMER_IDLE_TIMEOUT` for batched modes.
    pub fn with_idle_timeout(
        target: Arc<dyn DispatchTarget>,
        max_buffered_messages: usize,
        max_batch_tokens: usize,
        grouping: BatchGrouping,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            per_thread: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(0),
            target,
            max_buffered_messages,
            max_batch_tokens,
            grouping,
            idle_timeout,
        }
    }

    /// Build the dispatcher key for a (platform, thread, sender) tuple.
    ///
    /// In `Thread` mode the sender is ignored; in `Lane` mode the sender is appended
    /// so each (thread, sender) pair gets its own mpsc and consumer.
    ///
    /// Note: this is the *dispatcher* key, not the *session pool* key. Session pool keys
    /// are always `<platform>:<thread_id>` regardless of grouping (the ACP session is
    /// shared per-thread by design).
    pub fn key(&self, platform: &str, thread_id: &str, sender_id: &str) -> String {
        match self.grouping {
            BatchGrouping::Thread => format!("{platform}:{thread_id}"),
            BatchGrouping::Lane => format!("{platform}:{thread_id}:{sender_id}"),
        }
    }

    /// Build the shared session pool key for a routed channel.
    ///
    /// Unlike dispatcher keys, session keys never include sender identity.
    /// They track the logical conversation thread across all grouping modes.
    fn session_key(thread_channel: &ChannelRef) -> String {
        let logical_thread_id = thread_channel
            .thread_id
            .as_deref()
            .unwrap_or(&thread_channel.channel_id);
        format!("{}:{}", thread_channel.platform, logical_thread_id)
    }

    /// Submit one arrival event for the given thread.
    ///
    /// - If the thread has no active consumer, one is spawned lazily.
    /// - If the channel is full, this future parks until space is available
    ///   (backpressure — no data loss, no error).
    /// - If the consumer has died (`SendError`), surfaces ❌ + ⚠️ and returns
    ///   `Err(DispatchError::ConsumerDead)` (§2.5).
    ///
    /// `adapter` is passed per-call (not stored on `Dispatcher`) because the
    /// Discord adapter is constructed inside serenity's `ready` callback via
    /// `OnceLock` — after the Dispatcher is built in `main.rs`.
    pub async fn submit(
        &self,
        thread_key: String,
        thread_channel: ChannelRef,
        adapter: Arc<dyn ChatAdapter>,
        msg: BufferedMessage,
    ) -> Result<(), DispatchError> {
        let cap = self.max_buffered_messages;
        let target = Arc::clone(&self.target);
        let max_tokens = self.max_batch_tokens;
        let idle_timeout = self.idle_timeout;

        // Pre-fetch a generation in case we end up inserting a fresh handle.
        // Wasted if the entry already exists; generations need only be monotonic.
        let next_g = self.next_generation.fetch_add(1, Ordering::Relaxed);

        let (tx, my_generation) = {
            // SAFETY: no .await while this guard is held — guard drops at end of block.
            let mut map = self.per_thread.lock().unwrap();

            // Proactive stale-entry cleanup: if the consumer has exited (idle
            // timeout or unexpected), remove the entry so `or_insert_with`
            // creates a fresh one. Prevents map leak from one-shot thread keys
            // and avoids the first-message-after-idle being treated as an error.
            if let Some(handle) = map.get(&thread_key) {
                if handle.consumer.is_finished() {
                    map.remove(&thread_key);
                }
            }

            let entry = map.entry(thread_key.clone()).or_insert_with(|| {
                let (tx, rx) = tokio::sync::mpsc::channel(cap);
                let consumer = tokio::spawn(consumer_loop(
                    thread_key.clone(),
                    thread_channel.clone(),
                    rx,
                    Arc::clone(&target),
                    Arc::clone(&adapter),
                    cap,
                    max_tokens,
                    idle_timeout,
                ));
                ThreadHandle {
                    tx,
                    consumer,
                    generation: next_g,
                    channel_id: thread_channel.channel_id.clone(),
                    adapter_kind: adapter.platform().to_string(),
                }
            });
            (entry.tx.clone(), entry.generation)
        };

        if let Err(e) = tx.send(msg).await {
            // Consumer has exited between our check and the send — race-safe
            // eviction under lock (§2.5), then transparent retry once.
            //
            // Safe to re-acquire `per_thread` here: the first lock guard above
            // was dropped before `tx.send().await`, so this acquisition cannot
            // deadlock against the await point. The same property holds for the
            // retry acquisition below.
            {
                // SAFETY: no .await while this guard is held.
                let mut map = self.per_thread.lock().unwrap();
                Self::try_evict_locked(&mut map, &thread_key, my_generation);
            }
            let failed_msg = e.0;

            // Retry: spawn a fresh consumer and re-send. If this also fails,
            // surface the error to the user.
            let retry_g = self.next_generation.fetch_add(1, Ordering::Relaxed);
            let (retry_tx, retry_gen) = {
                // SAFETY: no .await while this guard is held — guard drops at end of block.
                let mut map = self.per_thread.lock().unwrap();
                let entry = map.entry(thread_key.clone()).or_insert_with(|| {
                    let (tx, rx) = tokio::sync::mpsc::channel(cap);
                    let consumer = tokio::spawn(consumer_loop(
                        thread_key.clone(),
                        thread_channel.clone(),
                        rx,
                        Arc::clone(&target),
                        Arc::clone(&adapter),
                        cap,
                        max_tokens,
                        idle_timeout,
                    ));
                    ThreadHandle {
                        tx,
                        consumer,
                        generation: retry_g,
                        channel_id: thread_channel.channel_id.clone(),
                        adapter_kind: adapter.platform().to_string(),
                    }
                });
                (entry.tx.clone(), entry.generation)
            };

            if let Err(e2) = retry_tx.send(failed_msg).await {
                // Retry also failed — truly unexpected. Surface error.
                {
                    // SAFETY: no .await while this guard is held.
                    let mut map = self.per_thread.lock().unwrap();
                    Self::try_evict_locked(&mut map, &thread_key, retry_gen);
                }
                let failed_msg = e2.0;
                let _ = adapter
                    .add_reaction(
                        &failed_msg.trigger_msg,
                        &self.target.reactions_config().emojis.error,
                    )
                    .await;
                let _ = adapter
                    .send_message(
                        &thread_channel,
                        &format!(
                            "⚠️ {}",
                            format_user_error("dispatch consumer exited unexpectedly")
                        ),
                    )
                    .await;
                return Err(DispatchError::ConsumerDead);
            }
        }
        Ok(())
    }

    /// Drop all per-thread handles whose key belongs to `(platform, thread_id)`,
    /// regardless of grouping, and abort each consumer (§2.5 / §4.4). Returns
    /// the total number of buffered messages discarded across all lanes.
    ///
    /// Matches both Thread keys (`<platform>:<thread_id>`) and Lane keys
    /// (`<platform>:<thread_id>:<sender_id>`). Used by `/reset` and
    /// `/cancel-all` to clear the entire thread, not just one lane.
    ///
    /// Disjoint from SendError recovery: removal happens *before* abort, so any
    /// fresh `submit` after this returns lands on a lazily-constructed new handle
    /// instead of observing `SendError`.
    pub fn cancel_buffered_thread(&self, platform: &str, thread_id: &str) -> usize {
        let prefix = format!("{platform}:{thread_id}");
        let lane_prefix = format!("{prefix}:");
        // SAFETY: no .await while this guard is held — function is sync.
        let mut map = self.per_thread.lock().unwrap();
        let keys: Vec<String> = map
            .keys()
            .filter(|k| k.as_str() == prefix || k.starts_with(&lane_prefix))
            .cloned()
            .collect();
        let mut dropped = 0;
        for k in keys {
            if let Some(handle) = map.remove(&k) {
                dropped += handle.pending_count();
                handle.consumer.abort();
            }
        }
        dropped
    }

    /// §2.5 race-safe eviction. Caller must hold the `per_thread` mutex.
    /// Removes the entry only if its generation matches `my_generation` —
    /// protects against evicting a fresh handle that another `submit` lazily
    /// inserted between this caller's failed `tx.send` and this call.
    /// Returns true if the entry was removed.
    fn try_evict_locked(
        map: &mut HashMap<String, ThreadHandle>,
        thread_key: &str,
        my_generation: u64,
    ) -> bool {
        if let Some(handle) = map.get(thread_key) {
            if handle.generation == my_generation {
                map.remove(thread_key);
                return true;
            }
        }
        false
    }

    /// Remove map entries whose consumer task has finished (idle timeout or
    /// unexpected exit). Called periodically from the cleanup task in main.rs
    /// to prevent unbounded map growth from one-shot thread keys that never
    /// receive a second `submit()`. Returns the number of entries swept.
    pub fn sweep_stale(&self) -> usize {
        // SAFETY: no .await while this guard is held — function is sync.
        let mut map = self.per_thread.lock().unwrap();
        let before = map.len();
        map.retain(|_, handle| !handle.consumer.is_finished());
        before - map.len()
    }

    /// Log buffered-message counts and drop all handles (called on SIGTERM).
    pub fn shutdown(&self) {
        // SAFETY: no .await while this guard is held — function is sync.
        let mut map = self.per_thread.lock().unwrap();
        for (thread_id, handle) in map.iter() {
            let pending = handle.pending_count();
            if pending > 0 {
                warn!(
                    thread_id = %thread_id,
                    channel   = %handle.channel_id,
                    adapter   = %handle.adapter_kind,
                    buffered_lost = pending,
                    "shutdown dropped pending messages without dispatch",
                );
            }
            handle.consumer.abort();
        }
        map.clear();
    }
}

// ---------------------------------------------------------------------------
// consumer_loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn consumer_loop(
    thread_key: String,
    thread_channel: ChannelRef,
    mut rx: tokio::sync::mpsc::Receiver<BufferedMessage>,
    target: Arc<dyn DispatchTarget>,
    adapter: Arc<dyn ChatAdapter>,
    max_batch: usize,
    max_tokens: usize,
    idle_timeout: Duration,
) {
    // `pending` holds a message that exceeded the token cap for the current batch;
    // it becomes the first message of the next batch, preserving FIFO.
    let mut pending: Option<BufferedMessage> = None;

    loop {
        // I1: block until at least one message arrives (zero latency for first message).
        // Idle timeout: if no message arrives within `idle_timeout` the consumer
        // exits, freeing the task and mpsc. The next `submit` for this thread_key
        // will observe `SendError`, evict the stale entry, and lazily spawn a
        // fresh consumer (§2.5 generation check prevents mis-eviction).
        let first = match pending.take() {
            Some(msg) => msg,
            None => match tokio::time::timeout(idle_timeout, rx.recv()).await {
                Ok(Some(msg)) => msg,
                Ok(None) => {
                    // All senders dropped → shutdown() or cancel_buffered_thread().
                    break;
                }
                Err(_elapsed) => {
                    debug!(
                        thread_key = %thread_key,
                        channel = %thread_channel.channel_id,
                        "consumer idle timeout, exiting"
                    );
                    break;
                }
            },
        };

        // Greedy drain up to max_batch messages or max_tokens.
        let mut batch = vec![first];
        let mut cumulative_tokens = batch[0].estimated_tokens;

        while batch.len() < max_batch {
            match rx.try_recv() {
                Ok(more) => {
                    if cumulative_tokens + more.estimated_tokens > max_tokens {
                        // Token cap — save for next turn (FIFO preserved).
                        pending = Some(more);
                        break;
                    }
                    cumulative_tokens += more.estimated_tokens;
                    batch.push(more);
                }
                Err(_) => break,
            }
        }

        // §2.6: read the freshest snapshot in the batch (batch is non-empty).
        let bot_present = batch.last().unwrap().other_bot_present;

        dispatch_batch(
            &thread_key,
            &thread_channel,
            &target,
            &adapter,
            batch,
            bot_present,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// dispatch_batch
// ---------------------------------------------------------------------------

async fn dispatch_batch(
    thread_key: &str,
    thread_channel: &ChannelRef,
    target: &Arc<dyn DispatchTarget>,
    adapter: &Arc<dyn ChatAdapter>,
    batch: Vec<BufferedMessage>,
    other_bot_present: bool,
) {
    let dispatch_start = Instant::now();
    let batch_size = batch.len();
    let session_key = Dispatcher::session_key(thread_channel);

    // Apply 👀 reaction to every message in the batch before dispatch (§6.7).
    // Sequential — batches are typically small (≤ low single digits) so the
    // serialization cost is sub-second and not user-visible; sequential keeps the
    // dispatch path free of `futures_util::join_all` and easier to reason about.
    let queued_emoji = &target.reactions_config().emojis.queued;
    for msg in batch.iter() {
        let _ = adapter.add_reaction(&msg.trigger_msg, queued_emoji).await;
    }

    // Collect per-event observability data (before consuming the batch).
    let tokens_per_event: Vec<usize> = batch.iter().map(|m| m.estimated_tokens).collect();
    let wait_ms: Vec<u128> = batch
        .iter()
        .map(|m| m.arrived_at.elapsed().as_millis())
        .collect();
    let senders: Vec<String> = batch.iter().map(|m| m.sender_name.clone()).collect();

    // Anchor reactions on the last message in the batch (before consuming).
    let trigger_msg = batch.last().unwrap().trigger_msg.clone();

    // Pack all arrival events into one Vec<ContentBlock> (§3.3).
    // Uses into_iter() to avoid deep-copying extra_blocks (may contain base64 image data).
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    for msg in batch {
        let mut event_blocks =
            AdapterRouter::pack_arrival_event(&msg.sender_json, &msg.prompt, msg.extra_blocks);
        content_blocks.append(&mut event_blocks);
    }
    let packed_block_count = content_blocks.len();

    // Ensure session exists.
    if let Err(e) = target.ensure_session(&session_key).await {
        let user_msg = format_user_error(&e.to_string());
        let _ = adapter
            .send_message(thread_channel, &format!("⚠️ {user_msg}"))
            .await;
        error!("pool error in dispatch_batch: {e}");
        return;
    }

    let reactions_config = target.reactions_config().clone();
    let reactions = Arc::new(StatusReactionController::new(
        reactions_config.enabled,
        adapter.clone(),
        trigger_msg,
        reactions_config.emojis.clone(),
        reactions_config.timing.clone(),
    ));
    // 👀 already applied above; skip set_queued() to avoid double-reaction.

    let result = target
        .stream_prompt_blocks(
            adapter,
            &session_key,
            content_blocks,
            thread_channel,
            reactions.clone(),
            other_bot_present,
        )
        .await;

    match &result {
        Ok(()) => reactions.set_done().await,
        Err(_) => reactions.set_error().await,
    }

    let hold_ms = if result.is_ok() {
        reactions_config.timing.done_hold_ms
    } else {
        reactions_config.timing.error_hold_ms
    };
    if reactions_config.remove_after_reply {
        let reactions = reactions;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
            reactions.clear().await;
        });
    }

    if let Err(ref e) = result {
        let _ = adapter
            .send_message(thread_channel, &format!("⚠️ {e}"))
            .await;
    }

    let agent_dispatch_ms = dispatch_start.elapsed().as_millis();
    let span = info_span!(
        "dispatch",
        channel = %thread_channel.channel_id,
        adapter = adapter.platform(),
    );
    let _enter = span.enter();
    info!(
        thread_key         = %thread_key,
        events_per_dispatch = batch_size,
        packed_block_count  = packed_block_count,
        agent_dispatch_ms   = agent_dispatch_ms,
        tokens_per_event    = ?tokens_per_event,
        wait_ms             = ?wait_ms,
        senders             = ?senders,
        "batch dispatched",
    );
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Rough char-to-token ratio for English-ish text. Coarse on purpose — the goal
/// is a guard rail for `max_batch_tokens`, not an exact pre-flight.
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;
/// Conservative per-image token budget. Larger than typical Claude image cost
/// so the cap trips before we hand the model an oversized batch.
const TOKENS_PER_IMAGE_ESTIMATE: usize = 512;

/// Rough token estimate for a buffered message (used for `max_batch_tokens` cap).
/// Intentionally coarse — the goal is a guard rail, not an exact pre-flight.
pub fn estimate_tokens(prompt: &str, extra_blocks: &[ContentBlock]) -> usize {
    let text_tokens = prompt.len() / CHARS_PER_TOKEN_ESTIMATE + 1;
    let block_tokens: usize = extra_blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len() / CHARS_PER_TOKEN_ESTIMATE + 1,
            ContentBlock::Image { .. } => TOKENS_PER_IMAGE_ESTIMATE,
        })
        .sum();
    text_tokens + block_tokens
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_empty() {
        assert!(estimate_tokens("", &[]) >= 1);
    }

    #[test]
    fn estimate_tokens_text() {
        // 400 chars ≈ 100 tokens
        let s = "a".repeat(400);
        assert_eq!(estimate_tokens(&s, &[]), 101);
    }

    #[test]
    fn estimate_tokens_image_block() {
        let blocks = vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "base64data".into(),
        }];
        assert_eq!(estimate_tokens("", &blocks), 1 + 512);
    }

    #[test]
    fn pack_arrival_event_single() {
        let blocks =
            AdapterRouter::pack_arrival_event(r#"{"schema":"openab.sender.v1"}"#, "hello", vec![]);
        // sender_context delimiter + prompt = 2 blocks
        assert_eq!(blocks.len(), 2);
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("<sender_context>"));
            assert!(text.contains("</sender_context>"));
            // Header is delimiter only — prompt lives in its own block.
            assert!(!text.contains("hello"));
        } else {
            panic!("expected Text delimiter block");
        }
        if let ContentBlock::Text { text } = &blocks[1] {
            assert_eq!(text, "hello");
        } else {
            panic!("expected Text prompt block");
        }
    }

    #[test]
    fn pack_arrival_event_with_extra_blocks() {
        let extra = vec![
            ContentBlock::Text {
                text: "[Voice transcript]: hi".into(),
            },
            ContentBlock::Image {
                media_type: "image/png".into(),
                data: "abc".into(),
            },
        ];
        let blocks = AdapterRouter::pack_arrival_event("{}", "prompt", extra);
        // delimiter + transcript + prompt + image = 4 blocks
        assert_eq!(blocks.len(), 4);
        assert!(
            matches!(&blocks[0], ContentBlock::Text { text } if text.contains("<sender_context>"))
        );
        assert!(
            matches!(&blocks[1], ContentBlock::Text { text } if text.contains("Voice transcript"))
        );
        assert!(matches!(&blocks[2], ContentBlock::Text { text } if text == "prompt"));
        assert!(matches!(&blocks[3], ContentBlock::Image { .. }));
    }

    #[test]
    fn pack_arrival_event_batch_n2() {
        // Two arrival events concatenated → 2 (header + prompt) pairs = 4 blocks.
        let mut all: Vec<ContentBlock> = Vec::new();
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"ts":"T1"}"#,
            "msg1",
            vec![],
        ));
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"ts":"T2"}"#,
            "msg2",
            vec![],
        ));
        assert_eq!(all.len(), 4);
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains(r#""ts":"T1""#));
            assert!(!text.contains("msg1"));
        }
        if let ContentBlock::Text { text } = &all[1] {
            assert_eq!(text, "msg1");
        }
        if let ContentBlock::Text { text } = &all[2] {
            assert!(text.contains(r#""ts":"T2""#));
            assert!(!text.contains("msg2"));
        }
        if let ContentBlock::Text { text } = &all[3] {
            assert_eq!(text, "msg2");
        }
    }

    // ADR §3.6 Scenario B — text in one message, image in the next, same author.
    // Broker preserves structural truth: image stays in M2 alone, both messages
    // carry the same sender_id so the agent can semantically link them.
    #[test]
    fn pack_arrival_event_scenario_b_image_in_separate_message() {
        let mut all: Vec<ContentBlock> = Vec::new();
        // M1 (alice): "see this image"
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T1"}"#,
            "see this image",
            vec![],
        ));
        // M2 (alice): image, no text
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T2"}"#,
            "",
            vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "imgB".into(),
            }],
        ));
        // header(M1) + prompt(M1) + header(M2) + image(M2) = 4 blocks
        // (M2 has empty prompt, so its prompt block is omitted)
        assert_eq!(all.len(), 4);
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains(r#""sender_id":"A""#));
            assert!(text.contains(r#""ts":"T1""#));
        } else {
            panic!("expected Text delimiter for M1");
        }
        if let ContentBlock::Text { text } = &all[1] {
            assert_eq!(text, "see this image");
        } else {
            panic!("expected Text prompt for M1");
        }
        if let ContentBlock::Text { text } = &all[2] {
            assert!(text.contains(r#""ts":"T2""#));
        } else {
            panic!("expected Text delimiter for M2");
        }
        // M2's image follows immediately after its delimiter (no prompt block).
        assert!(matches!(&all[3], ContentBlock::Image { .. }));
    }

    // ADR §3.6 Scenario C — fragmented multi-author batch.
    // Repeated sender_id is preserved across non-adjacent messages; bob's interjection
    // is kept as-is (no silent drop, no temporal reorder).
    #[test]
    fn pack_arrival_event_scenario_c_multi_author_interleaved() {
        let mut all: Vec<ContentBlock> = Vec::new();
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T1"}"#,
            "see this image",
            vec![],
        ));
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"B","ts":"T2"}"#,
            "what?",
            vec![],
        ));
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T3"}"#,
            "",
            vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "imgC".into(),
            }],
        ));
        // M1: header + prompt = 2 blocks
        // M2: header + prompt = 2 blocks
        // M3: header + image = 2 blocks (empty prompt → no prompt block)
        // total = 6
        assert_eq!(all.len(), 6);
        let h1 = match &all[0] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text delimiter for M1"),
        };
        let p1 = match &all[1] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text prompt for M1"),
        };
        let h2 = match &all[2] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text delimiter for M2"),
        };
        let p2 = match &all[3] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text prompt for M2"),
        };
        let h3 = match &all[4] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text delimiter for M3"),
        };
        assert!(h1.contains(r#""sender_id":"A""#) && h1.contains(r#""ts":"T1""#));
        assert_eq!(p1, "see this image");
        assert!(h2.contains(r#""sender_id":"B""#) && h2.contains(r#""ts":"T2""#));
        assert_eq!(p2, "what?");
        assert!(h3.contains(r#""sender_id":"A""#) && h3.contains(r#""ts":"T3""#));
        // M3's image attached to M3 only.
        assert!(matches!(&all[5], ContentBlock::Image { .. }));
    }

    // ADR §3.6 Scenario D — voice-only message in a batch.
    // Within each arrival, transcript Text blocks precede the prompt block so the
    // agent sees voice content before any typed text. The sender_context delimiter
    // still opens each arrival.
    #[test]
    fn pack_arrival_event_scenario_d_voice_only() {
        let mut all: Vec<ContentBlock> = Vec::new();
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T1"}"#,
            "look at this",
            vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "scr".into(),
            }],
        ));
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"A","ts":"T2"}"#,
            "",
            vec![ContentBlock::Text {
                text: "[Voice message transcript]: hey can we sync about the deploy".into(),
            }],
        ));
        all.extend(AdapterRouter::pack_arrival_event(
            r#"{"sender_id":"B","ts":"T3"}"#,
            "what?",
            vec![],
        ));
        // M1: header + prompt + image = 3
        // M2: header + transcript = 2 (empty prompt → no prompt block)
        // M3: header + prompt = 2
        // total = 7
        assert_eq!(all.len(), 7);
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains(r#""ts":"T1""#));
            assert!(!text.contains("look at this"));
        }
        if let ContentBlock::Text { text } = &all[1] {
            assert_eq!(text, "look at this");
        }
        assert!(matches!(&all[2], ContentBlock::Image { .. }));
        if let ContentBlock::Text { text } = &all[3] {
            assert!(text.contains(r#""ts":"T2""#));
        }
        // Transcript precedes prompt (and prompt is omitted here because empty).
        if let ContentBlock::Text { text } = &all[4] {
            assert!(text.contains("Voice message transcript"));
            assert!(text.contains("sync about the deploy"));
        } else {
            panic!("expected transcript Text block after M2 delimiter");
        }
        if let ContentBlock::Text { text } = &all[5] {
            assert!(text.contains(r#""sender_id":"B""#));
        }
        if let ContentBlock::Text { text } = &all[6] {
            assert_eq!(text, "what?");
        }
    }

    // Token-cap math: a single message that already exceeds max_batch_tokens still
    // dispatches alone (the consumer_loop logic admits the first message before
    // checking the cap). Verifies estimate_tokens scales with input length.
    #[test]
    fn estimate_tokens_oversized_single_message() {
        // ~24k token text (96000 chars / 4 chars-per-token).
        let big = "x".repeat(96_000);
        let est = estimate_tokens(&big, &[]);
        assert!(est > 24_000, "expected >24k tokens, got {est}");
    }

    // Cumulative token math: two messages whose sum exceeds max_batch_tokens.
    // The consumer_loop reads first, then peeks at the next; if cumulative tokens
    // > cap, the second is held over to the next batch (FIFO preserved).
    #[test]
    fn estimate_tokens_cumulative_exceeds_cap() {
        let max_tokens = 24_000_usize;
        let m1 = estimate_tokens(&"a".repeat(80_000), &[]);
        let m2 = estimate_tokens(&"b".repeat(50_000), &[]);
        assert!(m1 < max_tokens);
        assert!(m1 + m2 > max_tokens, "{m1} + {m2} should exceed cap");
    }

    // ADR §2.5 race-safe eviction. The full SendError path requires a real
    // AdapterRouter (concrete struct, not a trait — no easy mock seam), so we
    // unit-test the eviction predicate in isolation. End-to-end consumer-death
    // recovery is exercised by the manual staging smoke documented in the ADR.
    fn dummy_handle(generation: u64) -> ThreadHandle {
        let (tx, _rx) = tokio::sync::mpsc::channel::<BufferedMessage>(1);
        let consumer = tokio::spawn(async {});
        ThreadHandle {
            tx,
            consumer,
            generation,
            channel_id: "C".into(),
            adapter_kind: "discord".into(),
        }
    }

    #[tokio::test]
    async fn try_evict_locked_removes_when_generation_matches() {
        let mut map: HashMap<String, ThreadHandle> = HashMap::new();
        map.insert("t".into(), dummy_handle(7));
        assert!(Dispatcher::try_evict_locked(&mut map, "t", 7));
        assert!(map.is_empty());
    }

    // The bug §2.5 prevents: a stale producer (my_gen=7) observing SendError
    // must not remove a freshly inserted handle (gen=8) created by another
    // submit between the failed send and the eviction attempt.
    #[tokio::test]
    async fn try_evict_locked_keeps_when_generation_differs() {
        let mut map: HashMap<String, ThreadHandle> = HashMap::new();
        map.insert("t".into(), dummy_handle(8));
        assert!(!Dispatcher::try_evict_locked(&mut map, "t", 7));
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("t").unwrap().generation, 8);
    }

    #[tokio::test]
    async fn try_evict_locked_returns_false_when_absent() {
        let mut map: HashMap<String, ThreadHandle> = HashMap::new();
        assert!(!Dispatcher::try_evict_locked(&mut map, "missing", 0));
    }

    // BatchGrouping → thread_key shape.
    fn make_dispatcher(grouping: BatchGrouping) -> Dispatcher {
        // The router is wrapped in Arc but never used by `key()` itself; we use
        // a dummy AdapterRouter built via the same path main.rs would use.
        // For a pure-keying test we'd ideally not need it, but the constructor demands one.
        // Construct a minimal router via the public test helpers in adapter.rs if available;
        // otherwise we fall back to building one with a dummy SessionPool.
        use crate::acp::SessionPool;
        let agent_cfg = crate::config::AgentConfig {
            command: "/bin/true".into(),
            args: vec![],
            working_dir: "/tmp".into(),
            env: std::collections::HashMap::new(),
            inherit_env: vec![],
        };
        let pool = Arc::new(SessionPool::new(agent_cfg, 1));
        let router = Arc::new(AdapterRouter::new(
            pool,
            crate::config::ReactionsConfig::default(),
            crate::markdown::TableMode::Off,
            crate::config::default_prompt_hard_timeout_secs(),
            crate::config::default_liveness_check_secs(),
        ));
        Dispatcher::with_idle_timeout(router, 10, 24_000, grouping, DEFAULT_CONSUMER_IDLE_TIMEOUT)
    }

    #[tokio::test]
    async fn key_per_thread_ignores_sender() {
        let d = make_dispatcher(BatchGrouping::Thread);
        assert_eq!(d.key("discord", "T1", "userA"), "discord:T1");
        assert_eq!(d.key("discord", "T1", "userB"), "discord:T1");
    }

    #[tokio::test]
    async fn key_per_lane_includes_sender() {
        let d = make_dispatcher(BatchGrouping::Lane);
        assert_eq!(d.key("discord", "T1", "userA"), "discord:T1:userA");
        assert_eq!(d.key("discord", "T1", "userB"), "discord:T1:userB");
        // Different threads remain distinct.
        assert_eq!(d.key("slack", "T2", "userA"), "slack:T2:userA");
    }

    fn insert_dummy_handle(d: &Dispatcher, key: &str) {
        let (tx, _rx) = tokio::sync::mpsc::channel::<BufferedMessage>(10);
        let consumer = tokio::spawn(async {});
        let handle = ThreadHandle {
            tx,
            consumer,
            generation: 0,
            channel_id: "c".into(),
            adapter_kind: "discord".into(),
        };
        d.per_thread.lock().unwrap().insert(key.to_string(), handle);
    }

    #[tokio::test]
    async fn cancel_buffered_thread_drops_per_thread_key() {
        let d = make_dispatcher(BatchGrouping::Thread);
        insert_dummy_handle(&d, "discord:T1");
        insert_dummy_handle(&d, "discord:T2"); // different thread, must survive
        assert_eq!(d.cancel_buffered_thread("discord", "T1"), 0); // no buffered msgs
        let map = d.per_thread.lock().unwrap();
        assert!(!map.contains_key("discord:T1"));
        assert!(map.contains_key("discord:T2"));
    }

    #[tokio::test]
    async fn cancel_buffered_thread_drops_all_lanes() {
        let d = make_dispatcher(BatchGrouping::Lane);
        insert_dummy_handle(&d, "discord:T1:userA");
        insert_dummy_handle(&d, "discord:T1:userB");
        insert_dummy_handle(&d, "discord:T2:userA"); // different thread
        insert_dummy_handle(&d, "slack:T1:userA"); // different platform
        d.cancel_buffered_thread("discord", "T1");
        let map = d.per_thread.lock().unwrap();
        assert!(!map.contains_key("discord:T1:userA"));
        assert!(!map.contains_key("discord:T1:userB"));
        assert!(map.contains_key("discord:T2:userA"));
        assert!(map.contains_key("slack:T1:userA"));
    }

    #[tokio::test]
    async fn cancel_buffered_thread_does_not_match_thread_id_prefix() {
        // T1 must not match T10 / T11 (substring trap).
        let d = make_dispatcher(BatchGrouping::Lane);
        insert_dummy_handle(&d, "discord:T1:userA");
        insert_dummy_handle(&d, "discord:T10:userA");
        d.cancel_buffered_thread("discord", "T1");
        let map = d.per_thread.lock().unwrap();
        assert!(!map.contains_key("discord:T1:userA"));
        assert!(map.contains_key("discord:T10:userA"));
    }

    // Long-running consumer that parks until aborted — used by sweep_stale /
    // shutdown tests to exercise the "still alive" path.
    fn alive_consumer_handle() -> ThreadHandle {
        let (tx, _rx) = tokio::sync::mpsc::channel::<BufferedMessage>(10);
        let consumer = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        ThreadHandle {
            tx,
            consumer,
            generation: 0,
            channel_id: "c".into(),
            adapter_kind: "discord".into(),
        }
    }

    #[tokio::test]
    async fn sweep_stale_removes_finished_consumers() {
        let d = make_dispatcher(BatchGrouping::Thread);
        insert_dummy_handle(&d, "discord:T1");
        insert_dummy_handle(&d, "discord:T2");
        // Yield so the empty-body spawned tasks actually run to completion
        // before is_finished() is checked.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let swept = d.sweep_stale();
        assert_eq!(swept, 2);
        assert!(d.per_thread.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_stale_keeps_running_consumers() {
        let d = make_dispatcher(BatchGrouping::Thread);
        let abort = {
            let h = alive_consumer_handle();
            let a = h.consumer.abort_handle();
            d.per_thread.lock().unwrap().insert("alive".into(), h);
            a
        };
        let swept = d.sweep_stale();
        assert_eq!(swept, 0);
        assert!(d.per_thread.lock().unwrap().contains_key("alive"));
        // Cleanup so the parked task doesn't linger across tests.
        abort.abort();
    }

    #[tokio::test]
    async fn shutdown_clears_all_handles() {
        let d = make_dispatcher(BatchGrouping::Thread);
        insert_dummy_handle(&d, "k1");
        insert_dummy_handle(&d, "k2");
        insert_dummy_handle(&d, "k3");
        d.shutdown();
        assert!(d.per_thread.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn shutdown_aborts_running_consumers() {
        let d = make_dispatcher(BatchGrouping::Thread);
        let abort = {
            let h = alive_consumer_handle();
            let a = h.consumer.abort_handle();
            d.per_thread.lock().unwrap().insert("k".into(), h);
            a
        };
        d.shutdown();
        // Give the runtime a tick to process abort + map drop.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(abort.is_finished());
    }

    // -----------------------------------------------------------------------
    // consumer_loop / dispatch_batch integration tests (NIT 2)
    //
    // These drive `consumer_loop` directly with a pre-populated mpsc, using
    // `MockDispatchTarget` to record the calls that would otherwise hit a
    // real `AdapterRouter` (and through it, ACP CLI subprocesses). This
    // gives deterministic coverage of the orchestration paths the existing
    // unit tests don't reach: greedy drain, token-cap overflow, idle timeout.
    // -----------------------------------------------------------------------

    /// One recorded `stream_prompt_blocks` invocation.
    #[derive(Clone)]
    struct RecordedDispatch {
        block_count: usize,
        other_bot_present: bool,
    }

    /// Mock `DispatchTarget` — records calls; never touches a real session pool.
    struct MockDispatchTarget {
        reactions: ReactionsConfig,
        calls: Mutex<Vec<RecordedDispatch>>,
        /// If set, `ensure_session` returns this error once.
        ensure_err: Mutex<Option<String>>,
        /// If set, `stream_prompt_blocks` returns this error once.
        stream_err: Mutex<Option<String>>,
    }

    impl MockDispatchTarget {
        fn new() -> Self {
            Self {
                reactions: ReactionsConfig::default(),
                calls: Mutex::new(Vec::new()),
                ensure_err: Mutex::new(None),
                stream_err: Mutex::new(None),
            }
        }

        fn calls(&self) -> Vec<RecordedDispatch> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DispatchTarget for MockDispatchTarget {
        fn reactions_config(&self) -> &ReactionsConfig {
            &self.reactions
        }

        async fn ensure_session(&self, _session_key: &str) -> Result<()> {
            if let Some(msg) = self.ensure_err.lock().unwrap().take() {
                return Err(anyhow::anyhow!(msg));
            }
            Ok(())
        }

        async fn stream_prompt_blocks(
            &self,
            _adapter: &Arc<dyn ChatAdapter>,
            _session_key: &str,
            content_blocks: Vec<ContentBlock>,
            _thread_channel: &ChannelRef,
            _reactions: Arc<StatusReactionController>,
            other_bot_present: bool,
        ) -> Result<()> {
            self.calls.lock().unwrap().push(RecordedDispatch {
                block_count: content_blocks.len(),
                other_bot_present,
            });
            if let Some(msg) = self.stream_err.lock().unwrap().take() {
                return Err(anyhow::anyhow!(msg));
            }
            Ok(())
        }
    }

    /// Mock `ChatAdapter` — every method is a no-op success. The dispatch loop
    /// invokes `add_reaction` (queued 👀), `platform`, and on the error path
    /// `send_message`; nothing else needs real behavior here.
    struct MockChatAdapter;

    #[async_trait]
    impl ChatAdapter for MockChatAdapter {
        fn platform(&self) -> &'static str {
            "mock"
        }
        fn message_limit(&self) -> usize {
            2000
        }

        async fn send_message(&self, channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: "mock-msg".into(),
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
        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    fn make_channel(thread: &str) -> ChannelRef {
        ChannelRef {
            platform: "mock".into(),
            channel_id: thread.into(),
            thread_id: Some(thread.into()),
            parent_id: None,
            origin_event_id: None,
        }
    }

    fn make_msg(prompt: &str, tokens: usize) -> BufferedMessage {
        BufferedMessage {
            sender_json: r#"{"schema":"openab.sender.v1","sender_id":"u","sender_name":"u"}"#
                .into(),
            sender_name: "u".into(),
            prompt: prompt.into(),
            extra_blocks: vec![],
            trigger_msg: MessageRef {
                channel: make_channel("T"),
                message_id: format!("m-{prompt}"),
            },
            arrived_at: Instant::now(),
            estimated_tokens: tokens,
            other_bot_present: false,
        }
    }

    /// Pre-load `msgs` into a fresh mpsc, drop the sender, and run
    /// `consumer_loop` to completion. Returns the recorded dispatches.
    async fn run_consumer_with_messages(
        msgs: Vec<BufferedMessage>,
        max_batch: usize,
        max_tokens: usize,
    ) -> Vec<RecordedDispatch> {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(msgs.len().max(1));
        for m in msgs {
            tx.send(m).await.unwrap();
        }
        drop(tx);

        consumer_loop(
            "mock:T".into(),
            make_channel("T"),
            rx,
            target,
            adapter,
            max_batch,
            max_tokens,
            Duration::from_secs(60),
        )
        .await;

        mock.calls()
    }

    #[tokio::test]
    async fn consumer_dispatches_single_message_as_one_batch() {
        let calls = run_consumer_with_messages(vec![make_msg("hi", 10)], 10, 24_000).await;
        assert_eq!(calls.len(), 1);
        // pack_arrival_event with no extra_blocks → delimiter + prompt = 2 blocks.
        assert_eq!(calls[0].block_count, 2);
        assert!(!calls[0].other_bot_present);
    }

    #[tokio::test]
    async fn consumer_greedy_drain_combines_queued_messages_into_one_batch() {
        // 3 messages already in the queue when the consumer wakes → greedy
        // drain pulls all 3, packs them into one batch, dispatches once.
        let calls = run_consumer_with_messages(
            vec![make_msg("a", 50), make_msg("b", 50), make_msg("c", 50)],
            10,
            24_000,
        )
        .await;
        assert_eq!(calls.len(), 1, "expected a single batched dispatch");
        // 3 arrivals × (delimiter + prompt) = 6 blocks.
        assert_eq!(calls[0].block_count, 6);
    }

    #[tokio::test]
    async fn consumer_token_cap_splits_batch_preserving_fifo() {
        // max_tokens=100, two 80-token messages → cumulative 160 > 100, so
        // msg2 becomes `pending` and is dispatched in the next batch.
        let calls =
            run_consumer_with_messages(vec![make_msg("a", 80), make_msg("b", 80)], 10, 100).await;
        assert_eq!(calls.len(), 2, "token cap should split into two batches");
        // Each batch holds one arrival → delimiter + prompt = 2 blocks.
        assert_eq!(calls[0].block_count, 2);
        assert_eq!(calls[1].block_count, 2);
    }

    #[tokio::test]
    async fn consumer_exits_after_idle_timeout_with_no_messages() {
        // No messages ever arrive; consumer should exit once `idle_timeout`
        // elapses. Keep `tx` alive so the exit path is the timeout, not the
        // "all senders dropped" branch.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(1);
        let consumer = tokio::spawn(consumer_loop(
            "mock:T".into(),
            make_channel("T"),
            rx,
            target,
            adapter,
            10,
            24_000,
            Duration::from_millis(50),
        ));
        // Wait enough for the timeout branch + a tick for the task to finish.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            consumer.is_finished(),
            "consumer should exit after idle timeout"
        );
        // No dispatches should have been recorded.
        assert!(mock.calls().is_empty());
        drop(tx);
    }

    #[tokio::test]
    async fn submit_evicts_dead_handle_and_retries_with_fresh_consumer() {
        // §2.5: if `tx.send()` returns `SendError` (consumer's rx dropped
        // mid-flight), `submit` evicts the stale entry under lock and spawns
        // a fresh consumer. Manufacture this state by inserting a handle
        // whose consumer is still parked but whose rx has been dropped.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let d = Dispatcher::with_idle_timeout(
            target,
            10,
            24_000,
            BatchGrouping::Thread,
            DEFAULT_CONSUMER_IDLE_TIMEOUT,
        );
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);

        let key = "mock:T".to_string();
        let parked = {
            let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(10);
            drop(rx); // closes the channel → next tx.send() yields SendError
            let consumer = tokio::spawn(std::future::pending::<()>());
            let abort = consumer.abort_handle();
            let handle = ThreadHandle {
                tx,
                consumer,
                generation: 999,
                channel_id: "T".into(),
                adapter_kind: "mock".into(),
            };
            d.per_thread.lock().unwrap().insert(key.clone(), handle);
            abort
        };

        d.submit(key, make_channel("T"), adapter, make_msg("hello", 10))
            .await
            .expect("retry should spawn a fresh consumer");
        // Give the freshly spawned consumer time to drain + dispatch.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let calls = mock.calls();
        assert_eq!(
            calls.len(),
            1,
            "fresh consumer should have dispatched the retry"
        );
        // pack_arrival_event with no extra_blocks → delimiter + prompt = 2 blocks.
        assert_eq!(calls[0].block_count, 2);

        parked.abort();
    }
}

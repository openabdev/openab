//! Turn-boundary message batching dispatcher.
//!
//! See ADR: turn-boundary-batching-adr.md for full design rationale.
//!
//! # Invariants
//! - I1: First message after idle has zero added latency.
//! - I2: At most one in-flight ACP turn per thread.
//! - I3: Broker structural fidelity — no merging, splitting, reordering, or
//!   semantic transformation of arrival events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use tracing::{error, info, info_span, warn};

use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use crate::acp::ContentBlock;
use crate::error_display::format_user_error;
use crate::reactions::StatusReactionController;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One arrival event buffered for a future ACP turn.
pub struct BufferedMessage {
    /// Serialised SenderContext JSON (already built by the platform adapter).
    pub sender_json: String,
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
// Dispatcher
// ---------------------------------------------------------------------------

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
    router: Arc<AdapterRouter>,
    max_buffered_messages: usize,
    max_batch_tokens: usize,
}

impl Dispatcher {
    pub fn new(
        router: Arc<AdapterRouter>,
        max_buffered_messages: usize,
        max_batch_tokens: usize,
    ) -> Self {
        Self {
            per_thread: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(0),
            router,
            max_buffered_messages,
            max_batch_tokens,
        }
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
        let router = Arc::clone(&self.router);
        let max_tokens = self.max_batch_tokens;

        // Pre-fetch a generation in case we end up inserting a fresh handle.
        // Wasted if the entry already exists; generations need only be monotonic.
        let next_g = self.next_generation.fetch_add(1, Ordering::Relaxed);

        let (tx, my_generation) = {
            let mut map = self.per_thread.lock().unwrap();
            let entry = map.entry(thread_key.clone()).or_insert_with(|| {
                let (tx, rx) = tokio::sync::mpsc::channel(cap);
                let consumer = tokio::spawn(consumer_loop(
                    thread_key.clone(),
                    thread_channel.clone(),
                    rx,
                    Arc::clone(&router),
                    Arc::clone(&adapter),
                    cap,
                    max_tokens,
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
            // Consumer has exited — race-safe eviction under lock (§2.5).
            {
                let mut map = self.per_thread.lock().unwrap();
                Self::try_evict_locked(&mut map, &thread_key, my_generation);
            }
            let failed_msg = e.0;
            let _ = adapter
                .add_reaction(
                    &failed_msg.trigger_msg,
                    &self.router.reactions_config().emojis.error,
                )
                .await;
            let _ = adapter
                .send_message(
                    &thread_channel,
                    &format!("⚠️ {}", format_user_error("dispatch consumer exited unexpectedly")),
                )
                .await;
            return Err(DispatchError::ConsumerDead);
        }
        Ok(())
    }

    /// Drop the per-thread handle and abort its consumer, discarding any messages
    /// buffered at cancel time (§2.5 / §4.4). Returns the approximate number of
    /// messages that were buffered (mpsc length at removal time).
    ///
    /// Disjoint from SendError recovery: removal happens *before* abort, so any
    /// fresh `submit` after this returns lands on a lazily-constructed new handle
    /// instead of observing `SendError`.
    pub fn cancel_buffered(&self, thread_key: &str) -> usize {
        let mut map = self.per_thread.lock().unwrap();
        if let Some(handle) = map.remove(thread_key) {
            let pending = handle.pending_count();
            handle.consumer.abort();
            pending
        } else {
            0
        }
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

    /// Log buffered-message counts and drop all handles (called on SIGTERM).
    pub fn shutdown(&self) {
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
    router: Arc<AdapterRouter>,
    adapter: Arc<dyn ChatAdapter>,
    max_batch: usize,
    max_tokens: usize,
) {
    // `pending` holds a message that exceeded the token cap for the current batch;
    // it becomes the first message of the next batch, preserving FIFO.
    let mut pending: Option<BufferedMessage> = None;

    loop {
        // I1: block until at least one message arrives (zero latency for first message).
        let first = match pending.take() {
            Some(msg) => msg,
            None => match rx.recv().await {
                Some(msg) => msg,
                // All senders dropped → either shutdown() cleared the map, or
                // cancel_buffered() removed our entry. Exit the loop.
                None => break,
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
            &router,
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
    router: &Arc<AdapterRouter>,
    adapter: &Arc<dyn ChatAdapter>,
    batch: Vec<BufferedMessage>,
    other_bot_present: bool,
) {
    let dispatch_start = Instant::now();
    let batch_size = batch.len();

    // Apply 👀 reaction to every message in the batch before dispatch (§6.7).
    // Parallelized so first-token latency isn't paid for N serial reaction RPCs.
    let queued_emoji = &router.reactions_config().emojis.queued;
    futures_util::future::join_all(
        batch
            .iter()
            .map(|msg| adapter.add_reaction(&msg.trigger_msg, queued_emoji)),
    )
    .await;

    // Collect per-event observability data.
    let tokens_per_event: Vec<usize> = batch.iter().map(|m| m.estimated_tokens).collect();
    let wait_ms: Vec<u128> = batch
        .iter()
        .map(|m| m.arrived_at.elapsed().as_millis())
        .collect();

    // Pack all arrival events into one Vec<ContentBlock> (§3.3).
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    for msg in &batch {
        let mut event_blocks =
            AdapterRouter::pack_arrival_event(&msg.sender_json, &msg.prompt, msg.extra_blocks.clone());
        content_blocks.append(&mut event_blocks);
    }
    let packed_block_count = content_blocks.len();

    // Ensure session exists.
    if let Err(e) = router.pool().get_or_create(thread_key).await {
        let user_msg = format_user_error(&e.to_string());
        let _ = adapter
            .send_message(thread_channel, &format!("⚠️ {user_msg}"))
            .await;
        error!("pool error in dispatch_batch: {e}");
        return;
    }

    // Anchor reactions on the last message in the batch.
    let trigger_msg = batch.last().unwrap().trigger_msg.clone();
    let reactions_config = router.reactions_config().clone();
    let reactions = Arc::new(StatusReactionController::new(
        reactions_config.enabled,
        adapter.clone(),
        trigger_msg,
        reactions_config.emojis.clone(),
        reactions_config.timing.clone(),
    ));
    // 👀 already applied above; skip set_queued() to avoid double-reaction.

    let result = router
        .stream_prompt_blocks(
            adapter,
            thread_key,
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
        "batch dispatched",
    );
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Rough token estimate for a buffered message (used for `max_batch_tokens` cap).
/// Intentionally coarse — the goal is a guard rail, not an exact pre-flight.
pub fn estimate_tokens(prompt: &str, extra_blocks: &[ContentBlock]) -> usize {
    // ~4 chars per token for text; fixed 512 per image block (conservative).
    let text_tokens = prompt.len() / 4 + 1;
    let block_tokens: usize = extra_blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len() / 4 + 1,
            ContentBlock::Image { .. } => 512,
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
        let blocks = AdapterRouter::pack_arrival_event(
            r#"{"schema":"openab.sender.v1"}"#,
            "hello",
            vec![],
        );
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Text { text } = &blocks[0] {
            assert!(text.contains("<sender_context>"));
            assert!(text.contains("hello"));
        } else {
            panic!("expected Text block");
        }
    }

    #[test]
    fn pack_arrival_event_with_extra_blocks() {
        let extra = vec![
            ContentBlock::Text { text: "[Voice transcript]: hi".into() },
            ContentBlock::Image { media_type: "image/png".into(), data: "abc".into() },
        ];
        let blocks = AdapterRouter::pack_arrival_event("{}", "prompt", extra);
        // header + 2 extra = 3 blocks
        assert_eq!(blocks.len(), 3);
        // extra blocks follow the header in arrival order
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text.contains("Voice transcript")));
        assert!(matches!(&blocks[2], ContentBlock::Image { .. }));
    }

    #[test]
    fn pack_arrival_event_batch_n2() {
        // Two arrival events concatenated → 2 header blocks
        let mut all: Vec<ContentBlock> = Vec::new();
        all.extend(AdapterRouter::pack_arrival_event(r#"{"ts":"T1"}"#, "msg1", vec![]));
        all.extend(AdapterRouter::pack_arrival_event(r#"{"ts":"T2"}"#, "msg2", vec![]));
        assert_eq!(all.len(), 2);
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains("msg1"));
        }
        if let ContentBlock::Text { text } = &all[1] {
            assert!(text.contains("msg2"));
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
        // header(M1) + header(M2) + image(M2) = 3 blocks
        assert_eq!(all.len(), 3);
        // M1's header carries text only
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains(r#""sender_id":"A""#));
            assert!(text.contains(r#""ts":"T1""#));
            assert!(text.contains("see this image"));
        } else {
            panic!("expected Text header for M1");
        }
        // M2's header carries empty prompt (line after </sender_context> is blank)
        if let ContentBlock::Text { text } = &all[1] {
            assert!(text.contains(r#""ts":"T2""#));
            assert!(text.ends_with("\n\n"), "M2 prompt must be empty: {text:?}");
        } else {
            panic!("expected Text header for M2");
        }
        // M2's image follows immediately after its header (structural attribution)
        assert!(matches!(&all[2], ContentBlock::Image { .. }));
    }

    // ADR §3.6 Scenario C — fragmented multi-author batch.
    // Repeated sender_id is preserved across non-adjacent messages; bob's interjection
    // is kept as-is (no silent drop, no reorder).
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
        // 3 headers + 1 image = 4 blocks
        assert_eq!(all.len(), 4);
        // Order is preserved (no reorder).
        let h1 = match &all[0] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text"),
        };
        let h2 = match &all[1] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text"),
        };
        let h3 = match &all[2] {
            ContentBlock::Text { text } => text,
            _ => panic!("expected Text"),
        };
        assert!(h1.contains(r#""sender_id":"A""#) && h1.contains("see this image"));
        assert!(h2.contains(r#""sender_id":"B""#) && h2.contains("what?"));
        assert!(h3.contains(r#""sender_id":"A""#));
        // M3's image attached to M3 only.
        assert!(matches!(&all[3], ContentBlock::Image { .. }));
    }

    // ADR §3.6 Scenario D — voice-only message in a batch.
    // M2 has empty prompt + transcript text block. Per ADR, transcript moves AFTER
    // <sender_context> (vs. v0.8.2-beta.1's prepended position).
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
        // header(M1) + image(M1) + header(M2) + transcript(M2) + header(M3) = 5
        assert_eq!(all.len(), 5);
        if let ContentBlock::Text { text } = &all[0] {
            assert!(text.contains(r#""ts":"T1""#));
            assert!(text.contains("look at this"));
        }
        assert!(matches!(&all[1], ContentBlock::Image { .. }));
        // M2 header has empty prompt; transcript follows AFTER the header (not before).
        if let ContentBlock::Text { text } = &all[2] {
            assert!(text.contains(r#""ts":"T2""#));
            assert!(text.ends_with("\n\n"));
        }
        if let ContentBlock::Text { text } = &all[3] {
            assert!(text.contains("Voice message transcript"));
            assert!(text.contains("sync about the deploy"));
        } else {
            panic!("expected transcript Text block as M2 attachment");
        }
        if let ContentBlock::Text { text } = &all[4] {
            assert!(text.contains(r#""sender_id":"B""#));
            assert!(text.contains("what?"));
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
}

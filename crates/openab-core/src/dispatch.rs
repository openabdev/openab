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

use crate::workflow::identity::AgentIdentity;

use crate::acp::ContentBlock;
use crate::acp::ProjectContext;
use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use crate::agent_lease_heartbeat::{HeartbeatHandle, HeartbeatProducer};
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
    /// Slack streaming recipient `(user_id, team_id)` for `chat.startStream`,
    /// captured at message-arrival time (after allow-list) and bound to this turn
    /// — no shared thread cache, so no cross-turn race. Populated for real-user
    /// Slack turns regardless of `assistant_mode`; only *consumed* when assistant
    /// mode's native streaming is active. `None` for non-Slack platforms and
    /// bot-authored turns.
    pub recipient: Option<(String, String)>,
    /// Optional correlation from the future native admission path. Discord and
    /// other existing transports leave this empty. The freshest admitted event
    /// carries it through the canonical turn to the post-ACP completion hook.
    pub native_workflow: Option<crate::admission::NativeWorkflowMetadata>,
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

    /// Workspace aliases from config (for `[[ws:@alias]]` resolution).
    fn workspace_aliases(&self) -> std::collections::HashMap<String, String>;

    /// Bot home directory (security boundary for workspace resolution).
    fn bot_home(&self) -> std::path::PathBuf;

    /// Ensure the ACP session for `session_key` exists (idempotent).
    /// Returns `true` if a new session was created, `false` if it already existed.
    ///
    /// `project` carries the transport-neutral project identity (workflow
    /// `20260818-openab-project-scoped-acp-session-bootstrap`):
    /// - `Some(p)` with non-empty `p.project_id` pins the session to
    ///   `(project_id, project_root)`; an existing session for the same key
    ///   with a different binding is rejected.
    /// - `Some(ProjectContext::anonymous(path))` flows the legacy
    ///   `[[ws:@alias]]` workspace hint through the same seam without
    ///   pinning.
    /// - `None` defers to the configured `[agent].working_dir`.
    ///
    /// `write_policy` — Phase 6.4.1F: when the session is created fresh
    /// from a native-work dispatch, the pool applies the supplied
    /// policy to the ACP connection's `WritePolicyGuard` BEFORE any
    /// tool call can be observed. `None` preserves pre-6.4.1F
    /// behaviour (no tool-permission gate). Non-native turns always
    /// pass `None`.
    async fn ensure_session(
        &self,
        session_key: &str,
        project: Option<&ProjectContext>,
        write_policy: Option<&str>,
    ) -> Result<bool>;

    /// Destroy the session for `session_key` (used to rollback on directive failure).
    async fn reset_session(&self, session_key: &str);

    /// Return the canonical absolute path of the **pinned** project
    /// for `session_key`, or `None` if no project-pinned session
    /// currently exists. Used by the OpenAB-native A13 workflow-role
    /// gate and the `<workflow_context>` injector to resolve
    /// `<project_root>/.openab/workflow_assignment.json` without
    /// falling back to daemon cwd or workspace aliases.
    ///
    /// Anonymous workspace hints (legacy `[[ws:...]]`) deliberately
    /// do not pin and therefore return `None`.
    async fn pinned_project_root(&self, session_key: &str) -> Option<std::path::PathBuf>;

    /// Return the set of Discord numeric user IDs authorised as the
    /// Tech Lead for the A13 workflow-role bypass path. Sourced
    /// from `[workflow] tech_lead_user_ids` in `config.toml`. Empty
    /// means no Tech Lead bypass is available — ordinary humans
    /// will *not* be admitted when a workflow is active and they
    /// are not in this set. Production deployments must populate
    /// this; the OpenAB config schema documents the field.
    fn tech_lead_user_ids(&self) -> std::collections::HashSet<u64>;

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
        recipient: Option<(String, String)>,
    ) -> Result<((), Option<crate::workflow::service::WorkflowTurnHookInputs>)>;

    /// Phase 4.1: borrow the configured `WorkflowService` so the
    /// dispatch layer can invoke `on_turn_complete` AFTER the
    /// ACP turn has fully completed and the `AcpConnection`
    /// borrow has been released. `None` = no service configured
    /// (legacy no-op preserved).
    fn workflow_service(&self) -> Option<Arc<crate::workflow::service::WorkflowService>>;

    fn native_completion_port(&self) -> crate::native_completion::SharedNativeCompletionPort;

    /// Phase 6.4: optional AAP autonomous ingress client. `None`
    /// preserves legacy `WORKFLOW_ASSIGNMENT_MISSING` → ordinary ACP
    /// behavior. Production wiring injects a real HTTP client.
    fn autonomous_ingress_client(
        &self,
    ) -> Option<Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>>;

    /// Phase 6.4: optional AAP autonomous ingress config. `None`
    /// preserves legacy behavior.
    fn autonomous_ingress_config(&self) -> Option<&crate::config::AutonomousIngressConfig>;

    /// Phase 6.4: read-only access to the daemon's logical agent identity
    /// (resolved from `ARTHUR_AGENT_NAME`). `None` when env is unset,
    /// empty, or unknown. Used by the A13 gate's autonomous routing
    /// branch to check membership in `aap_agents`.
    fn autonomous_ingress_agent_identity(&self) -> Option<&str>;

    /// Observation point at the existing post-ACP completion boundary.
    /// Production targets retain the no-op default; focused tests can record
    /// the complete typed carrier without invoking a workflow transition.
    async fn observe_workflow_turn_hook(
        &self,
        _hook: &crate::workflow::service::WorkflowTurnHookInputs,
    ) {
    }
}

#[async_trait]
impl DispatchTarget for AdapterRouter {
    fn reactions_config(&self) -> &ReactionsConfig {
        AdapterRouter::reactions_config(self)
    }

    fn workspace_aliases(&self) -> std::collections::HashMap<String, String> {
        self.workspace_aliases_map()
    }

    fn bot_home(&self) -> std::path::PathBuf {
        self.bot_home_path()
    }

    async fn ensure_session(
        &self,
        session_key: &str,
        project: Option<&ProjectContext>,
        write_policy: Option<&str>,
    ) -> Result<bool> {
        let created = self.pool().get_or_create(session_key, project).await?;
        // Phase 6.4.1F — for fresh native-work sessions, apply the
        // supplied `write_policy` to the ACP connection's guard
        // BEFORE the pool yields. Re-applying on `created_now == false`
        // is harmless (the guard is idempotent) and keeps the seam
        // uniform across first-create and reused-session paths so a
        // re-dispatch with a stricter policy cannot leak through a
        // prior lenient session.
        if let Some(policy) = write_policy {
            self.pool()
                .set_session_write_policy(session_key, policy)
                .await;
        }
        Ok(created)
    }

    async fn reset_session(&self, session_key: &str) {
        let _ = self.pool().reset_session(session_key).await;
    }

    async fn pinned_project_root(&self, session_key: &str) -> Option<std::path::PathBuf> {
        self.pool()
            .get_pinned_project(session_key)
            .await
            .map(|p| p.project_root)
    }

    fn tech_lead_user_ids(&self) -> std::collections::HashSet<u64> {
        self.configured_tech_lead_user_ids()
    }

    async fn stream_prompt_blocks(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        session_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
        other_bot_present: bool,
        recipient: Option<(String, String)>,
    ) -> Result<((), Option<crate::workflow::service::WorkflowTurnHookInputs>)> {
        AdapterRouter::stream_prompt_blocks(
            self,
            adapter,
            session_key,
            content_blocks,
            thread_channel,
            reactions,
            other_bot_present,
            recipient,
        )
        .await
    }

    fn workflow_service(&self) -> Option<Arc<crate::workflow::service::WorkflowService>> {
        AdapterRouter::workflow_service(self)
    }
    fn native_completion_port(&self) -> crate::native_completion::SharedNativeCompletionPort {
        AdapterRouter::native_completion_port(self)
    }

    fn autonomous_ingress_client(
        &self,
    ) -> Option<Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>> {
        AdapterRouter::autonomous_ingress_client(self)
    }

    fn autonomous_ingress_config(&self) -> Option<&crate::config::AutonomousIngressConfig> {
        AdapterRouter::autonomous_ingress_config(self)
    }

    fn autonomous_ingress_agent_identity(&self) -> Option<&str> {
        AdapterRouter::resolved_agent_name(self)
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
    /// Phase 6.4.x — OpenAB-native agent lease heartbeat producer.
    /// ``None`` keeps the legacy behavior (no heartbeat, AAP TTL
    /// recovery is the only lease lifetime authority). When set,
    /// the dispatcher starts a heartbeat task at the "native
    /// dispatch turn starting" boundary for every accepted
    /// native dispatch and stops it at every terminal path
    /// (completion / failure / cancellation) so a finished turn
    /// cannot keep the lease alive past the scheduler's reclaim
    /// window.
    heartbeat_producer: Option<Arc<HeartbeatProducer>>,
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
            heartbeat_producer: None,
        }
    }

    /// Attach a heartbeat producer. Production wiring calls this
    /// once at composition time when the AAP runtime URL +
    /// credential are both present. The dispatcher drops the
    /// reference if ``producer`` is ``None`` (legacy behavior).
    pub fn with_heartbeat_producer(mut self, producer: Option<Arc<HeartbeatProducer>>) -> Self {
        self.heartbeat_producer = producer;
        self
    }

    /// Phase 6.4.x — start a heartbeat for a native dispatch.
    /// ``None`` producer → no-op (legacy behavior).
    #[allow(dead_code)]
    fn start_native_heartbeat(
        &self,
        metadata: &crate::admission::NativeWorkflowMetadata,
    ) -> Option<HeartbeatHandle> {
        self.heartbeat_producer
            .as_ref()
            .map(|producer| producer.start(metadata))
    }

    /// Narrow test-only ownership proof: the dispatcher must retain the exact
    /// AdapterRouter assembled by composition, rather than a parallel router.
    #[cfg(test)]
    pub(crate) fn targets_router(&self, router: &Arc<AdapterRouter>) -> bool {
        let target: Arc<dyn DispatchTarget> = router.clone();
        Arc::ptr_eq(&self.target, &target)
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
    ///
    /// Delegates to `ChannelRef::session_pool_key()` so the ctl layer
    /// (`RuntimeHandler::ensure_pinned_project`) and the dispatcher produce
    /// byte-identical keys for the same thread (workflow
    /// `20260818-openab-project-aware-thread-routing` test M).
    fn session_key(thread_channel: &ChannelRef) -> String {
        thread_channel.session_pool_key()
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
                    self.heartbeat_producer.clone(),
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
                        self.heartbeat_producer.clone(),
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
    heartbeat_producer: Option<Arc<HeartbeatProducer>>,
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

        // Build the dispatch batch.
        //
        // Native-workflow messages carry fenced execution authority
        // (`NativeWorkflowMetadata`) and MUST own their dispatch batch
        // exclusively — never co-batched with ordinary turns and never
        // merged with a different native fenced dispatch. Otherwise
        // `dispatch_batch` would derive the whole-batch authority from
        // `batch.last().native_workflow` (the freshest known state) and
        // either (a) collapse a native turn into an ordinary turn (native
        // authority loss, completion hook cannot resolve) or (b) collapse
        // two distinct native fenced dispatches into one authority
        // (authority confusion).
        //
        // Rule:
        //   - First message is native  → singleton dispatch (no greedy
        //     drain; FIFO still preserved because no other message has
        //     been consumed for this batch).
        //   - First message is ordinary → greedy drain as before, but the
        //     drain STOPS at the first native message; that native message
        //     is parked in `pending` so the next loop iteration dispatches
        //     it as its own singleton batch.
        //   - Token-cap overflow continues to use `pending` for ordinary
        //     messages (unchanged).
        let mut batch = vec![first];
        let mut cumulative_tokens = batch[0].estimated_tokens;
        let first_is_native = batch[0].native_workflow.is_some();

        if !first_is_native {
            while batch.len() < max_batch {
                match rx.try_recv() {
                    Ok(more) => {
                        // First native message encountered during an
                        // ordinary greedy drain must be preserved verbatim
                        // and dispatched as its own singleton batch in the
                        // next loop iteration. This keeps native authority
                        // bound to its own turn and avoids contaminating
                        // the ordinary batch with a foreign fenced dispatch.
                        if more.native_workflow.is_some() {
                            pending = Some(more);
                            break;
                        }
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
        }

        // §2.6: read the freshest snapshot in the batch (batch is non-empty).
        let bot_present = batch.last().unwrap().other_bot_present;

        dispatch_batch(
            &thread_key,
            &thread_channel,
            &target,
            heartbeat_producer.as_ref(),
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

/// Best-effort extraction of the author's `is_bot` flag from a
/// `BufferedMessage`'s `sender_json`. The sender JSON is built by
/// `build_sender_context` and contains an `"is_bot": bool` field;
/// serde parsing is bounded — any unexpected shape falls back to
/// `false` so the A12 multibot semantics continue to hold.
fn parse_sender_is_bot(sender_json: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct SenderJsonMin {
        #[serde(default)]
        is_bot: bool,
    }
    serde_json::from_str::<SenderJsonMin>(sender_json)
        .map(|s| s.is_bot)
        .unwrap_or(false)
}

/// Best-effort extraction of the author's display name from
/// `sender_json`. Used for log readability only — the gate's
/// authorisation decision keys on `user_id` (see
/// `parse_sender_user_id_from_json`), NEVER on display name.
/// Display-name impersonation attacks fail closed by construction
/// because `tech_lead_identities` is keyed on the immutable numeric
/// `user_id`.
fn parse_sender_display_name(sender_json: &str) -> String {
    #[derive(serde::Deserialize)]
    struct SenderJsonMin {
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        sender_name: String,
    }
    serde_json::from_str::<SenderJsonMin>(sender_json)
        .map(|s| {
            if !s.display_name.is_empty() {
                s.display_name
            } else {
                s.sender_name
            }
        })
        .unwrap_or_default()
}

/// Build the structured "A13 workflow-role gate" tracing record.
/// Fields:
/// - `sender_id`: numeric Discord user id of the inbound sender
///   (or `<unknown>` when unparseable),
/// - `sender_is_bot`: `true` when the inbound message was authored
///   by a Discord bot account,
/// - `tech_lead_authorized`: `true` when `sender_id` is in the
///   deployment's `[workflow] tech_lead_user_ids` set,
/// - `agent_identity`: the daemon's logical name (or `<unknown>`),
/// - `resolved_role`: the role this daemon fills in the assignment
///   (or `<none>`),
/// - `workflow_id`: the workflow id of the loaded assignment (or
///   `<none>`),
/// - `workflow_stage`: the assignment's stage (or `<none>`),
/// - `decision`: `admit` or `suppress`,
/// - `reason`: the [`GateReason`] token,
/// - `context_present`: `true` if a `<workflow_context>` block was
///   prepended.
///
/// No secrets are logged — workflow_id and stage are non-secret
/// opaque identifiers.
#[allow(clippy::too_many_arguments)]
fn log_a13_trace(
    decision: &crate::workflow::context::GateDecision,
    a13_context_text: &Option<String>,
    session_key: &str,
    pinned_root: Option<&std::path::Path>,
    sender_user_id: Option<u64>,
    sender_is_bot: bool,
    tech_lead_authorized: bool,
) {
    use crate::workflow::context::GateDecision;
    let (decision_str, reason_token) = match decision {
        GateDecision::Admit { reason } => ("admit", reason.as_str()),
        GateDecision::Suppress { reason, .. } => ("suppress", reason.as_str()),
    };
    let identity = crate::workflow::identity::current_agent_identity_from_env()
        .ok()
        .map(|i| i.as_str().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    // Best-effort: peek at the assignment for richer trace fields.
    // Without an assignment, surface `<none>` so logs are stable
    // across runs.
    let assignment = pinned_root.and_then(|p| {
        crate::workflow::assignment::load_assignment(p)
            .ok()
            .flatten()
    });
    let workflow_id = assignment
        .as_ref()
        .map(|a| a.workflow_id.clone())
        .unwrap_or_else(|| "<none>".to_string());
    let workflow_stage = assignment
        .as_ref()
        .map(|a| a.state.to_string())
        .unwrap_or_else(|| "<none>".to_string());

    let resolved_role: String = match crate::workflow::identity::current_agent_identity_from_env()
        .ok()
        .and_then(|id| {
            assignment
                .as_ref()
                .and_then(|a| crate::workflow::identity::resolve_role_from_assignment(id, a).ok())
        }) {
        Some(r) => r.role.to_string(),
        None => "<none>".to_string(),
    };

    info!(
        target: "openab::a13",
        session_key = %session_key,
        sender_id = sender_user_id.map(|u| u.to_string()).unwrap_or_else(|| "<unknown>".to_string()),
        sender_is_bot = sender_is_bot,
        tech_lead_authorized = tech_lead_authorized,
        agent_identity = %identity,
        resolved_role = %resolved_role,
        workflow_id = %workflow_id,
        workflow_stage = %workflow_stage,
        decision = %decision_str,
        reason = %reason_token,
        context_present = a13_context_text.is_some(),
        "A13 workflow-role gate",
    );
}

async fn dispatch_batch(
    thread_key: &str,
    thread_channel: &ChannelRef,
    target: &Arc<dyn DispatchTarget>,
    heartbeat_producer: Option<&Arc<HeartbeatProducer>>,
    adapter: &Arc<dyn ChatAdapter>,
    batch: Vec<BufferedMessage>,
    other_bot_present: bool,
) {
    let dispatch_start = Instant::now();
    let batch_size = batch.len();
    // Phase 6.2.9 — native-work dispatch isolation (FIX ROUND 4 root cause).
    //
    // The legacy `session_key` derivation below uses `thread_channel.session_pool_key()`
    // which produces `discord:<channel_id>`. That is the correct ACP session-pool
    // key for *human Discord* turns, but it is the WRONG key for native-work
    // dispatches admitted via `set agent.work`: those MUST use
    // `native-dispatch:<agent>:<dispatch_id>` (computed by the ctl handler and
    // carried on `NativeWorkflowMetadata.native_execution_session_key`) so the
    // pool's `create_fresh_session_only` fast lane activates, no historical
    // ACP turns for that Discord channel are replayed, and the dispatcher
    // thread key matches the ctl-side `agent:conversation_key:dispatch_id`
    // ledger key.
    //
    // The Discord delivery target (`thread_channel.channel_id`) remains the
    // transport-only reply destination; it is NEVER reused as the ACP
    // session-pool key on the native-work path.
    //
    // Invariant: for every native `agent.work` admission, the SessionPool key
    // MUST be `native-dispatch:<agent>:<dispatch_id>`. An empty string is
    // treated as "absent" so a malformed native-work payload cannot silently
    // collide with the legacy `discord:` pool key space.
    let session_key = batch
        .last()
        .and_then(|m| m.native_workflow.as_ref())
        .and_then(|nw| nw.native_execution_session_key.as_deref())
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Dispatcher::session_key(thread_channel));

    // Phase 6.2.9 — production observability for native ACP session
    // acquisition. Emitted at the dispatch_batch boundary so production
    // smoke can prove the actual key the SessionPool sees. No prompt /
    // payload / secret content is logged; the IDs are redacted by the
    // pool's own logger where appropriate, but here we log them raw
    // because they are the explicit production correlation handle for
    // this round and contain no payload.
    if let Some(nw) = batch
        .last()
        .and_then(|m| m.native_workflow.as_ref())
        .filter(|nw| nw.native_execution_session_key.is_some())
    {
        info!(
            dispatch_id = %nw.dispatch_id,
            agent = %nw.agent,
            execution_session_key = %session_key,
            delivery_channel_id = %thread_channel.channel_id,
            native = true,
            "native ACP session acquisition"
        );
    }

    // Apply 👀 reaction to every message in the batch before dispatch (§6.7).
    // Skip when assistant status API is active — uses
    // assistant.threads.setStatus instead of emoji reactions.
    let assistant_status = adapter.uses_assistant_status();
    if !assistant_status {
        let queued_emoji = &target.reactions_config().emojis.queued;
        for msg in batch.iter() {
            let _ = adapter.add_reaction(&msg.trigger_msg, queued_emoji).await;
        }
    }

    // Collect per-event observability data (before consuming the batch).
    let tokens_per_event: Vec<usize> = batch.iter().map(|m| m.estimated_tokens).collect();
    let wait_ms: Vec<u128> = batch
        .iter()
        .map(|m| m.arrived_at.elapsed().as_millis())
        .collect();
    let senders: Vec<String> = batch.iter().map(|m| m.sender_name.clone()).collect();

    // Native-streaming recipient is bound to the turn (captured per-message). A
    // batch attributes to the most recent sender; None for non-Slack/bot turns.
    let recipient: Option<(String, String)> = batch.last().and_then(|m| m.recipient.clone());
    let native_workflow = batch.last().and_then(|m| m.native_workflow.clone());

    // Phase 6.2.8: structured native dispatch turn boundary tracing.
    // One production native turn can be correlated end-to-end by emitting
    // `workflow_run_id`, `dispatch_id`, `lease_generation`, and `role` at
    // every observation site. No prompt / raw assistant text / credentials
    // are logged here — the goal is diagnostic correlation, not payload
    // capture.
    //
    // Phase 6.4.x — start the OpenAB-native agent lease heartbeat
    // task at the "accepted native dispatch" boundary so a
    // long-running turn does not let AAP's ``expire_stale`` sweep
    // reclaim the lease mid-execution and trigger a duplicate
    // redispatch. The handle is dropped into
    // ``native_heartbeat_handle`` and stopped at every terminal
    // path below (completion / failure / cancellation) so a
    // finished turn cannot keep the lease alive past the
    // scheduler's reclaim window.
    let mut native_heartbeat_handle: Option<HeartbeatHandle> = None;
    if let Some(metadata) = native_workflow.as_ref() {
        info!(
            workflow_run_id   = %metadata.workflow_run_id,
            dispatch_id       = %metadata.dispatch_id,
            lease_generation  = metadata.lease_generation,
            role              = %metadata.role,
            "native dispatch turn starting"
        );
        native_heartbeat_handle = heartbeat_producer.map(|producer| producer.start(metadata));
    }

    // Anchor reactions on the last message in the batch (before consuming).
    let trigger_msg = batch.last().unwrap().trigger_msg.clone();
    let dispatch_channel = ChannelRef {
        // Reply correlation is event-scoped, but the dispatcher consumer is
        // thread-scoped. Rebuild the per-dispatch channel from the stable
        // thread route plus the freshest event ID so gateway replies (e.g.
        // LINE reply-token lookup) target the current inbound event.
        origin_event_id: trigger_msg.channel.origin_event_id.clone(),
        ..thread_channel.clone()
    };

    // Pack all arrival events into one Vec<ContentBlock> (§3.3).
    // Uses into_iter() to avoid deep-copying extra_blocks (may contain base64 image data).
    let mut content_blocks: Vec<ContentBlock> = Vec::new();

    // Parse control directives from the first message in the batch (ADR: control-directives).
    // Directives are only processed on the session's first message (§2.2).
    //
    // Strategy:
    //   1. Parse directives (cheap text extraction — no mutation, no I/O)
    //   2. Attempt workspace resolution if [[ws:...]] present (may fail gracefully)
    //   3. Call ensure_session with resolved workspace — returns created_now
    //   4. Only strip prompt and apply title/workspace if created_now == true
    //   5. If created_now == false, the [[...]] text is preserved verbatim
    let mut batch = batch;
    let parse_result = batch
        .first()
        .map(|first_msg| crate::directives::parse_directives(&first_msg.prompt));

    // Tentatively resolve [[ws:...]] — if resolution fails and the session turns out to
    // be new, we abort. If the session already existed, resolution failure is irrelevant.
    let ws_resolved: Option<Result<String, String>> = parse_result.as_ref().and_then(|pr| {
        pr.metadata.raw.get("ws").map(|ws_value| {
            let aliases = target.workspace_aliases();
            let bot_home = target.bot_home();
            crate::directives::resolve_workspace(ws_value, &aliases, &bot_home)
                .map(|p| p.display().to_string())
        })
    });

    // Extract workspace path for ensure_session (None if no directive or resolution failed).
    // Wrap as an anonymous ProjectContext so the legacy `[[ws:@alias]]` directive
    // flows through the project-context seam without binding a project_id. The
    // pool's immutability invariant for anonymous contexts (stored > anonymous
    // path > config) preserves the existing per-thread workspace stickiness.
    let project_override: Option<ProjectContext> = ws_resolved
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|path| ProjectContext::anonymous(std::path::PathBuf::from(path)));

    // Ensure session exists. The create_gate mutex inside get_or_create serializes
    // concurrent callers — only the winner gets created_now == true.
    //
    // Phase 6.4.1F — for native-work turns the structured scope's
    // `write_policy` is forwarded to the pool so the ACP layer can
    // apply deterministic denial at `session/request_permission`
    // BEFORE the tool can mutate the filesystem. Non-native turns
    // pass `None` (the pool keeps the pre-6.4.1F auto-allow path).
    let write_policy_token: Option<&str> = native_workflow
        .as_ref()
        .and_then(|nw| nw.scope_policy.as_ref())
        .map(|p| {
            if p.is_read_only() {
                "READ_ONLY"
            } else {
                "MODIFY_ALLOWED"
            }
        });
    let created_now = match target
        .ensure_session(&session_key, project_override.as_ref(), write_policy_token)
        .await
    {
        Ok(created) => created,
        Err(e) => {
            let user_msg = format_user_error(&e.to_string());
            let _ = adapter
                .send_message(&dispatch_channel, &format!("⚠️ {user_msg}"))
                .await;
            error!("pool error in dispatch_batch: {e}");
            return;
        }
    };

    // Only apply directives if this is genuinely the first message (fresh session).
    if created_now {
        if let Some(pr) = parse_result {
            if !pr.metadata.raw.is_empty() {
                // Apply [[title:...]] independently — works regardless of ws outcome.
                let title_to_apply = pr.metadata.title.clone();

                // If workspace resolution failed on a NEW session, rollback and abort.
                // Reset FIRST to minimize TOCTOU window (擺渡 F1), then rename.
                if let Some(Err(e)) = ws_resolved {
                    target.reset_session(&session_key).await;
                    // Apply title after reset so the thread is identifiable.
                    if let Some(ref title) = title_to_apply {
                        if !title.is_empty() {
                            let _ = adapter.rename_thread(&dispatch_channel, title).await;
                        }
                    }
                    let _ = adapter
                        .send_message(&dispatch_channel, &format!("⚠️ {e}"))
                        .await;
                    error!(session_key, error = %e, "workspace directive rejected");
                    return;
                }

                // Strip directives from the prompt
                if let Some(first_msg) = batch.first_mut() {
                    first_msg.prompt = pr.prompt;
                }

                // Apply title on success path.
                if let Some(ref title) = title_to_apply {
                    if !title.is_empty() {
                        if let Err(e) = adapter.rename_thread(&dispatch_channel, title).await {
                            warn!(session_key, error = %e, "failed to apply title directive");
                        }
                    }
                }
            }
        }
    }

    // OpenAB-native A13 workflow-role gate. Pure logic lives in
    // `crate::workflow::context::phase3_a13_decide`; we just read
    // its verdict, emit the structured "A13 workflow-role gate"
    // trace line, and either suppress the dispatch or inject the
    // rendered `<workflow_context>` block as the FIRST content
    // block in the ACP prompt. The original user message is
    // preserved byte-for-byte downstream of the prepended workflow
    // context.
    //
    // Phase 3 deliberately does NOT mutate the ledger, the
    // assignment, or dispatch the next handoff message. The
    // dispatch.rs change here is a wiring surface only.
    //
    // Snapshot author_is_bot from the first message BEFORE we move
    // `batch` into the iter loop below — `batch` is consumed by the
    // loop so we cannot index into it afterwards.
    if let Some(metadata) = native_workflow.as_ref() {
        // Native work is admitted under AAP's fenced WorkflowRun + AgentLease
        // authority. Never read or reconcile legacy assignment projections for
        // this turn: they may describe a different workflow or role.
        content_blocks.insert(
            0,
            ContentBlock::Text {
                text: crate::admission::render_native_workflow_authority(metadata),
            },
        );
    } else {
        let first_msg = batch.first().unwrap();
        let author_is_bot = parse_sender_is_bot(&first_msg.sender_json);
        let sender_user_id =
            crate::workflow::context::parse_sender_user_id_from_json(&first_msg.sender_json);
        let sender_display_name = parse_sender_display_name(&first_msg.sender_json);
        let pinned_root = target.pinned_project_root(&session_key).await;
        let tech_lead_user_ids = target.tech_lead_user_ids();
        let (a13_decision, a13_context_text) = crate::workflow::context::phase3_a13_decide(
            pinned_root.as_deref(),
            sender_user_id,
            sender_display_name.as_str(),
            author_is_bot,
            &session_key,
            &tech_lead_user_ids,
        );
        log_a13_trace(
            &a13_decision,
            &a13_context_text,
            &session_key,
            pinned_root.as_deref(),
            sender_user_id,
            author_is_bot,
            crate::workflow::context::is_tech_lead_authorized(
                sender_user_id
                    .map(|uid| crate::workflow::SenderIdentity {
                        user_id: uid,
                        display_name: sender_display_name.clone(),
                    })
                    .as_ref(),
                author_is_bot,
                &tech_lead_user_ids,
            ),
        );
        if a13_decision.is_suppress() {
            info!(
                thread_key = %session_key,
                channel = %thread_channel.channel_id,
                reason = a13_decision.reason().as_str(),
                "A13 suppressed dispatch",
            );
            return;
        }

        // Phase 6.4: deterministic OpenAB → AAP autonomous ingress
        // routing. When the A13 gate admits because no
        // `workflow_assignment.json` exists, AND the operator has
        // declared this daemon's logical agent as AAP-autonomous
        // capable, AND the inbound human sender is Tech-Lead-authorized
        // (or `aap_universal_humans = true`), the dispatch loop MUST
        // consult AAP Runtime BEFORE proceeding to ordinary ACP.
        //
        // This is the only place where the routing contract is
        // evaluated. It deliberately does not inspect the prompt body,
        // does not consult the LLM, and does not perform any NLP
        // keyword matching. Acceptance from AAP consumes the human
        // turn; any failure mode (rejected / unavailable / auth) fails
        // closed without falling back to ordinary ACP.
        if a13_decision.reason() == crate::workflow::context::GateReason::WorkflowAssignmentMissing
        {
            let aap_cfg = target.autonomous_ingress_config();
            let aap_client = target.autonomous_ingress_client();
            let agent_name = target.autonomous_ingress_agent_identity();
            let tech_lead_authorized = crate::workflow::context::is_tech_lead_authorized(
                sender_user_id
                    .map(|uid| crate::workflow::SenderIdentity {
                        user_id: uid,
                        display_name: sender_display_name.clone(),
                    })
                    .as_ref(),
                author_is_bot,
                &tech_lead_user_ids,
            );
            if crate::autonomous_ingress::should_route_to_aap(
                aap_cfg,
                agent_name.unwrap_or(""),
                tech_lead_authorized,
            ) {
                let Some(client) = aap_client else {
                    warn!(
                        thread_key = %session_key,
                        channel = %thread_channel.channel_id,
                        agent = agent_name.unwrap_or(""),
                        "autonomous ingress configured but client missing; fail-closed",
                    );
                    return;
                };
                let candidate = crate::autonomous_ingress::build_candidate(
                    agent_name.unwrap_or(""),
                    thread_channel,
                    trigger_msg.message_id.as_str(),
                );
                crate::autonomous_ingress::log_candidate(&candidate);
                let prompt_text = batch.first().map(|m| m.prompt.clone()).unwrap_or_default();
                let request = crate::autonomous_ingress::AutonomousIngressRequest {
                    protocol: "openab",
                    project_id: aap_cfg.map(|c| c.project_id.clone()).unwrap_or_default(),
                    transport: "DISCORD",
                    conversation_key: thread_channel.session_pool_key(),
                    user_objective: prompt_text,
                    trace_id: session_key.to_string(),
                    task_id: None,
                    primary_agent: agent_name.unwrap_or("").to_string(),
                    language: "en".to_string(),
                    metadata: crate::autonomous_ingress::AutonomousIngressMetadata {
                        discord_message_id: Some(trigger_msg.message_id.clone()),
                        discord_channel_id: Some(thread_channel.channel_id.clone()),
                        discord_thread_id: thread_channel.thread_id.clone(),
                        discord_user_id: sender_user_id.map(|u| u.to_string()),
                        discord_sender_is_bot: author_is_bot,
                        // Phase 6.4.1D — mirror the structured
                        // delivery destination into the metadata
                        // block so the AAP `_coerce_delivery_destination`
                        // promotion point picks it up. The top-level
                        // `delivery_destination` field below is the
                        // wire-of-record; this metadata mirror
                        // preserves the OpenClaw-bridge-style shape.
                        delivery_destination: Some(
                            crate::autonomous_ingress::AutonomousIngressDeliveryDestination {
                                platform: thread_channel.platform.clone(),
                                channel_id: thread_channel.channel_id.clone(),
                                thread_id: thread_channel.thread_id.clone(),
                                parent_id: thread_channel.parent_id.clone(),
                                origin_event_id: Some(trigger_msg.message_id.clone()),
                            },
                        ),
                    },
                    // Phase 6.4.1D — authoritative structured delivery
                    // destination sourced from the trusted
                    // `thread_channel: ChannelRef` at the dispatch site.
                    // AAP mirrors this into the AAP-side
                    // `OpenABAutonomousIngressRequestModel.delivery_destination`
                    // field and propagates it through the binding so
                    // scheduler hops send `AgentWorkRequest.delivery_destination`
                    // with the actual workflow's originating channel.
                    delivery_destination: Some(
                        crate::autonomous_ingress::AutonomousIngressDeliveryDestination {
                            platform: thread_channel.platform.clone(),
                            channel_id: thread_channel.channel_id.clone(),
                            thread_id: thread_channel.thread_id.clone(),
                            parent_id: thread_channel.parent_id.clone(),
                            origin_event_id: Some(trigger_msg.message_id.clone()),
                        },
                    ),
                };
                let submit_result = client.submit(request).await;
                let disposition = match submit_result {
                    Ok(resp) => crate::autonomous_ingress::project_response(
                        resp,
                        &thread_channel.session_pool_key(),
                    ),
                    Err(err) => crate::autonomous_ingress::project_error(err),
                };
                match &disposition {
                    crate::autonomous_ingress::AutonomousRouteDisposition::Accepted { .. } => {
                        crate::autonomous_ingress::log_accepted(&candidate, &disposition);
                        info!(
                            thread_key = %session_key,
                            channel = %thread_channel.channel_id,
                            agent = %candidate.agent,
                            "Phase 6.4: human turn consumed by AAP autonomous ingress; ordinary ACP dispatch suppressed",
                        );
                        return;
                    }
                    _ => {
                        crate::autonomous_ingress::log_failure(&candidate, &disposition);
                        warn!(
                            thread_key = %session_key,
                            channel = %thread_channel.channel_id,
                            agent = %candidate.agent,
                            disposition = ?disposition,
                            "Phase 6.4: AAP autonomous ingress failed; fail-closed without ordinary ACP fallback",
                        );
                        return;
                    }
                }
            }
        }

        if let Some(ref block_text) = a13_context_text {
            content_blocks.insert(
                0,
                ContentBlock::Text {
                    text: block_text.clone(),
                },
            );
        }
    }

    for msg in batch {
        let mut event_blocks =
            AdapterRouter::pack_arrival_event(&msg.sender_json, &msg.prompt, msg.extra_blocks);
        content_blocks.append(&mut event_blocks);
    }
    let packed_block_count = content_blocks.len();

    let reactions_config = target.reactions_config().clone();
    let reactions = Arc::new(StatusReactionController::new(
        reactions_config.enabled,
        adapter.clone(),
        trigger_msg,
        reactions_config.emojis.clone(),
        reactions_config.timing.clone(),
    ));
    // 👀 already applied above; skip set_queued() to avoid double-reaction.

    let mut result = target
        .stream_prompt_blocks(
            adapter,
            &session_key,
            content_blocks,
            &dispatch_channel,
            reactions.clone(),
            other_bot_present,
            recipient,
        )
        .await;

    // Phase 6.2.8: log exactly which post-ACP result shape occurred. The
    // shape drives whether `invoke_workflow_hook_after_dispatch` runs and
    // therefore whether the native completion hook fires. Ordinary turns
    // already have their own observability via the "batch dispatched" log
    // below; this branch is diagnostic-only for native turns so the trail
    // is correlated by `workflow_run_id` / `dispatch_id` / `lease_generation`
    // / `role`.
    if let Some(metadata) = native_workflow.as_ref() {
        match &result {
            Ok(((), Some(hook))) => {
                info!(
                    workflow_run_id   = %metadata.workflow_run_id,
                    dispatch_id       = %metadata.dispatch_id,
                    lease_generation  = metadata.lease_generation,
                    role              = %metadata.role,
                    stop_reason       = ?hook.stop_reason,
                    terminal          = hook.terminal,
                    shape             = "ok-hook",
                    "native dispatch turn: stream_prompt_blocks returned Ok(Some(hook))"
                );
            }
            Ok(((), None)) => {
                info!(
                    workflow_run_id   = %metadata.workflow_run_id,
                    dispatch_id       = %metadata.dispatch_id,
                    lease_generation  = metadata.lease_generation,
                    role              = %metadata.role,
                    shape             = "ok-no-hook",
                    "native dispatch turn: stream_prompt_blocks returned Ok(None)"
                );
            }
            Err(error) => {
                warn!(
                    workflow_run_id   = %metadata.workflow_run_id,
                    dispatch_id       = %metadata.dispatch_id,
                    lease_generation  = metadata.lease_generation,
                    role              = %metadata.role,
                    shape             = "error",
                    error             = %error,
                    "native dispatch turn: stream_prompt_blocks returned Err"
                );
            }
        }
    }

    // Phase 6.4.x — stop the heartbeat at every terminal path
    // (Ok(Some(hook)), Ok(None), and Err). The canonical
    // completion flow drives ``AgentLeaseService.release`` on the
    // AAP side independently of this stop; we only need the
    // heartbeat to fall silent so a finished turn cannot keep the
    // lease alive past the scheduler's reclaim window. We
    // ``take`` the handle so the subsequent ``Drop`` impl does
    // not redundantly signal stop after ``await`` returns — the
    // task is already joined.
    if let Some(handle) = native_heartbeat_handle.take() {
        handle.stop().await;
    }

    // In assistant status mode, all status is conveyed via
    // assistant.threads.setStatus — skip emoji reactions entirely.
    if !assistant_status {
        match &result {
            Ok(((), _)) => reactions.set_done().await,
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
    }

    // Phase 4.1: invoke the workflow hook only after
    // `stream_prompt_blocks` has returned, which means the
    // `SessionPool::with_connection` borrow has ended. This must not be
    // nested under the reaction UI branch: assistant-status transports still
    // complete ACP turns and require the same workflow transition handling.
    if let Ok((_, Some(hook))) = &mut result {
        hook.native_workflow = native_workflow;
        invoke_workflow_hook_after_dispatch(target, hook).await;
    }

    if let Err(ref e) = result {
        let _ = adapter
            .send_message(&dispatch_channel, &format!("⚠️ {e}"))
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

/// Phase 4.1: invoke the configured `WorkflowService::on_turn_complete`
/// outside the ACP `Box::pin` scope, after the AcpConnection borrow
/// has been released. This is the only path that calls into the
/// workflow service from the dispatch layer.
///
/// Failures of the workflow hook NEVER affect the normal Discord
/// response delivery — the streaming turn has already completed by
/// the time this runs, and the hook has its own persistence +
/// messenger discipline. Errors here are logged and discarded.
async fn invoke_workflow_hook_after_dispatch(
    target: &Arc<dyn DispatchTarget>,
    hook: &crate::workflow::service::WorkflowTurnHookInputs,
) {
    if let Some(metadata) = hook.native_workflow.as_ref() {
        // Phase 6.2.8: log entry into the native post-turn boundary so a
        // production native turn is correlated with every later observation
        // (outcome resolution, port submission, failure). Structured fields
        // match the other sites in this file.
        info!(
            workflow_run_id   = %metadata.workflow_run_id,
            dispatch_id       = %metadata.dispatch_id,
            lease_generation  = metadata.lease_generation,
            role              = %metadata.role,
            stop_reason       = ?hook.stop_reason,
            terminal          = hook.terminal,
            "native workflow hook: entering post-turn boundary"
        );

        // Native turns never enter the prose/Discord workflow authority.
        if hook.stop_reason.as_deref() == Some("end_turn") {
            let Some(outcome) = crate::native_completion::resolve_native_completion_outcome(
                &metadata.role,
                &hook.raw_assistant_text,
            ) else {
                tracing::warn!(
                    workflow_run_id   = %metadata.workflow_run_id,
                    dispatch_id       = %metadata.dispatch_id,
                    lease_generation  = metadata.lease_generation,
                    role              = %metadata.role,
                    "native terminal turn has no unambiguous canonical role outcome; not capturing completion"
                );
                return;
            };
            // Phase 6.2.8: log the resolved canonical completion outcome.
            info!(
                workflow_run_id   = %metadata.workflow_run_id,
                dispatch_id       = %metadata.dispatch_id,
                lease_generation  = metadata.lease_generation,
                role              = %metadata.role,
                outcome           = %outcome,
                "native workflow hook: resolved canonical completion outcome"
            );
            let event = crate::native_completion::NativeCompletionEvent {
                record_version: 1,
                completion_id: String::new(),
                captured_at: String::new(),
                record_digest: String::new(),
                source: "openab".into(),
                dispatch_id: metadata.dispatch_id.clone(),
                conversation_key: metadata.conversation_key.clone(),
                workflow_run_id: metadata.workflow_run_id.clone(),
                task_id: metadata.task_id.clone(),
                role: metadata.role.clone(),
                agent_identity: metadata.agent.clone(),
                lease_id: metadata.lease_id.clone(),
                lease_generation: metadata.lease_generation,
                expected_revision: metadata.expected_revision,
                outcome: outcome.clone(),
                session_id: hook.session_key.clone(),
                openab_turn_id: metadata.dispatch_id.clone(),
                language: metadata.language.clone(),
                raw_assistant_text: hook.raw_assistant_text.clone(),
                project_id: metadata.project_id.clone().unwrap_or_default(),
                project_root: metadata.project_root.clone().unwrap_or_default(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                // Phase 6.4.1B — propagate authoritative transport from the
                // trusted structured dispatch metadata (carried by the
                // `agent.work` admission) into the completion event so AAP
                // Runtime's transport-aware conversation identity validator
                // (Phase 6.4.1) can match. `None` means the daemon build did
                // not plumb transport through `agent.work`; Runtime then
                // falls back to legacy OPENAB semantics.
                transport: metadata.transport.clone(),
            };
            match target.native_completion_port().submit(event).await {
                Ok(()) => {
                    // Phase 6.2.8: log successful submission to the native
                    // completion port — closes the post-turn trail.
                    info!(
                        workflow_run_id   = %metadata.workflow_run_id,
                        dispatch_id       = %metadata.dispatch_id,
                        lease_generation  = metadata.lease_generation,
                        role              = %metadata.role,
                        outcome           = %outcome,
                        "native workflow hook: completion event submitted to port"
                    );
                }
                Err(error) => {
                    // Phase 6.2.8: preserve the existing failure log line
                    // (verbatim message) and lift it onto the same structured
                    // correlation fields as the surrounding trace.
                    tracing::warn!(
                        workflow_run_id   = %metadata.workflow_run_id,
                        dispatch_id       = %metadata.dispatch_id,
                        lease_generation  = metadata.lease_generation,
                        role              = %metadata.role,
                        outcome           = %outcome,
                        error             = %error,
                        "native completion callback failed"
                    );
                }
            }
        }
        return;
    }
    if hook.terminal {
        target.observe_workflow_turn_hook(hook).await;
    }
    let Some(service) = target.workflow_service() else {
        // Legacy behaviour preserved: no service configured = no
        // workflow processing. This is also the test-mock path.
        return;
    };
    if !hook.terminal {
        // Non-terminal stop_reason (e.g. mid-stream pause).
        // Workflow processing only runs for terminal stops.
        return;
    }
    let pinned_ref = hook.pinned_project_root.as_deref();
    let outcome = service
        .on_turn_complete(
            &hook.session_key,
            pinned_ref,
            &hook.channel,
            &hook.raw_assistant_text,
            true,
        )
        .await;
    info!(
        session_key = %hook.session_key,
        agent_identity = ?hook.agent_identity.map(AgentIdentity::as_str),
        stop_reason = ?hook.stop_reason,
        outcome = ?outcome,
        "workflow hook completed",
    );
}

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
            command_explicit: true,
        };
        let pool = Arc::new(SessionPool::with_test_state(
            agent_cfg,
            crate::acp::pool::SessionPoolTestState::default(),
            std::path::PathBuf::from("/tmp/session_projects.json"),
        ));
        let router = Arc::new(AdapterRouter::new(
            pool,
            crate::config::ReactionsConfig::default(),
            crate::markdown::TableMode::Off,
            crate::config::default_prompt_hard_timeout_secs(),
            crate::config::default_liveness_check_secs(),
            std::collections::HashMap::new(),
            std::path::PathBuf::from("/tmp"),
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
        text_blocks: Vec<String>,
        other_bot_present: bool,
        dispatch_channel: ChannelRef,
        /// The session-pool key that `dispatch_batch` derived and passed into
        /// `ensure_session` for this turn. Phase 6.2.9 FIX ROUND 4 tests assert
        /// this field is `native-dispatch:<agent>:<dispatch_id>` for native
        /// work and `discord:<channel>[:<thread>]` for human Discord turns.
        session_key: String,
    }

    /// Mock `DispatchTarget` — records calls; never touches a real session pool.
    struct MockDispatchTarget {
        reactions: ReactionsConfig,
        calls: Mutex<Vec<RecordedDispatch>>,
        hook_inputs: Mutex<Vec<crate::workflow::service::WorkflowTurnHookInputs>>,
        native_events: Arc<Mutex<Vec<crate::native_completion::NativeCompletionEvent>>>,
        /// If set, `ensure_session` returns this error once.
        ensure_err: Mutex<Option<String>>,
        /// If set, `stream_prompt_blocks` returns this error once.
        stream_err: Mutex<Option<String>>,
        /// If set, `stream_prompt_blocks` returns this hook instead of `None`.
        /// Tests that exercise the post-ACP completion path inject a hook here.
        next_hook: Mutex<Option<crate::workflow::service::WorkflowTurnHookInputs>>,
        /// Phase 6.2.9 FIX ROUND 4 — every `ensure_session` call pushes the
        /// session-pool key it was invoked with. Tests assert this vector to
        /// prove the actual key the SessionPool would receive.
        session_keys_seen: Mutex<Vec<String>>,
        /// Phase 6.4: optional AAP autonomous ingress client injected
        /// into the dispatch target. Tests use this to assert accept /
        /// reject / unavailable / auth-missing behavior.
        autonomous_ingress_client:
            Option<Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>>,
        /// Phase 6.4: optional AAP autonomous ingress config.
        autonomous_ingress_config: Option<crate::config::AutonomousIngressConfig>,
        /// Phase 6.4: optional daemon agent identity override (when not
        /// driven by `ARTHUR_AGENT_NAME`).
        autonomous_ingress_agent_identity: Option<String>,
        /// Phase 6.4: Tech Lead user-id set consulted by the A13 gate's
        /// `is_tech_lead_authorized` check.
        tech_lead_user_ids: std::collections::HashSet<u64>,
    }

    impl MockDispatchTarget {
        fn new() -> Self {
            Self {
                reactions: ReactionsConfig::default(),
                calls: Mutex::new(Vec::new()),
                hook_inputs: Mutex::new(Vec::new()),
                native_events: Arc::new(Mutex::new(Vec::new())),
                ensure_err: Mutex::new(None),
                stream_err: Mutex::new(None),
                next_hook: Mutex::new(None),
                session_keys_seen: Mutex::new(Vec::new()),
                autonomous_ingress_client: None,
                autonomous_ingress_config: None,
                autonomous_ingress_agent_identity: None,
                tech_lead_user_ids: std::collections::HashSet::new(),
            }
        }

        fn calls(&self) -> Vec<RecordedDispatch> {
            self.calls.lock().unwrap().clone()
        }

        /// Returns the session-pool keys seen by `ensure_session`, in arrival
        /// order. Phase 6.2.9 FIX ROUND 4 focused tests use this to assert
        /// the actual key the SessionPool receives, not just that some key
        /// was passed (which is what prior tests verified, allowing the bug
        /// to slip through).
        fn session_keys(&self) -> Vec<String> {
            self.session_keys_seen.lock().unwrap().clone()
        }

        /// Inject the hook that `stream_prompt_blocks` will return on the
        /// next call. Used by integration tests that drive the dispatcher +
        /// completion seam end-to-end without hand-constructing
        /// `WorkflowTurnHookInputs` outside the dispatcher.
        fn set_next_hook(&self, hook: crate::workflow::service::WorkflowTurnHookInputs) {
            *self.next_hook.lock().unwrap() = Some(hook);
        }

        /// Phase 6.4: wire the autonomous ingress client and config into
        /// the mock target so tests can drive accept / reject / unavailable
        /// / auth-missing paths.
        fn with_autonomous_ingress(
            mut self,
            client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>,
            config: crate::config::AutonomousIngressConfig,
            agent: &str,
        ) -> Self {
            self.autonomous_ingress_client = Some(client);
            self.autonomous_ingress_config = Some(config);
            self.autonomous_ingress_agent_identity = Some(agent.to_string());
            self
        }

        /// Phase 6.4: configure the Tech-Lead user-id set consulted by
        /// the A13 gate's `is_tech_lead_authorized` check.
        fn with_tech_lead_user_ids(mut self, ids: std::collections::HashSet<u64>) -> Self {
            self.tech_lead_user_ids = ids;
            self
        }
    }

    #[async_trait]
    impl DispatchTarget for MockDispatchTarget {
        fn reactions_config(&self) -> &ReactionsConfig {
            &self.reactions
        }

        fn workspace_aliases(&self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }

        fn bot_home(&self) -> std::path::PathBuf {
            std::path::PathBuf::from("/tmp")
        }

        async fn ensure_session(
            &self,
            session_key: &str,
            _project: Option<&ProjectContext>,
            _write_policy: Option<&str>,
        ) -> Result<bool> {
            self.session_keys_seen
                .lock()
                .unwrap()
                .push(session_key.to_string());
            if let Some(msg) = self.ensure_err.lock().unwrap().take() {
                return Err(anyhow::anyhow!(msg));
            }
            Ok(true)
        }

        async fn reset_session(&self, _session_key: &str) {}

        async fn pinned_project_root(&self, _session_key: &str) -> Option<std::path::PathBuf> {
            None
        }

        fn tech_lead_user_ids(&self) -> std::collections::HashSet<u64> {
            self.tech_lead_user_ids.clone()
        }

        async fn stream_prompt_blocks(
            &self,
            _adapter: &Arc<dyn ChatAdapter>,
            session_key: &str,
            content_blocks: Vec<ContentBlock>,
            thread_channel: &ChannelRef,
            _reactions: Arc<StatusReactionController>,
            other_bot_present: bool,
            _recipient: Option<(String, String)>,
        ) -> Result<((), Option<crate::workflow::service::WorkflowTurnHookInputs>)> {
            // Phase 6.2.9 FIX ROUND 4 — record the session-pool key alongside
            // the dispatch so focused tests can verify it matches the
            // `native-dispatch:<agent>:<dispatch_id>` invariant. (Prior to
            // this, `_session_key` was ignored, which is exactly why the bug
            // slipped through: tests verified that *something* was passed
            // but never what.)
            self.calls.lock().unwrap().push(RecordedDispatch {
                block_count: content_blocks.len(),
                text_blocks: content_blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        ContentBlock::Image { .. } => None,
                    })
                    .collect(),
                other_bot_present,
                dispatch_channel: thread_channel.clone(),
                session_key: session_key.to_string(),
            });
            if let Some(msg) = self.stream_err.lock().unwrap().take() {
                return Err(anyhow::anyhow!(msg));
            }
            let hook = self.next_hook.lock().unwrap().take();
            Ok(((), hook))
        }

        fn workflow_service(&self) -> Option<Arc<crate::workflow::service::WorkflowService>> {
            None
        }

        fn native_completion_port(&self) -> crate::native_completion::SharedNativeCompletionPort {
            Arc::new(RecordingNativeCompletionPort(self.native_events.clone()))
        }

        fn autonomous_ingress_client(
            &self,
        ) -> Option<Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>> {
            self.autonomous_ingress_client.clone()
        }

        fn autonomous_ingress_config(&self) -> Option<&crate::config::AutonomousIngressConfig> {
            self.autonomous_ingress_config.as_ref()
        }

        fn autonomous_ingress_agent_identity(&self) -> Option<&str> {
            self.autonomous_ingress_agent_identity.as_deref()
        }

        async fn observe_workflow_turn_hook(
            &self,
            hook: &crate::workflow::service::WorkflowTurnHookInputs,
        ) {
            self.hook_inputs.lock().unwrap().push(hook.clone());
        }
    }

    struct RecordingNativeCompletionPort(
        Arc<Mutex<Vec<crate::native_completion::NativeCompletionEvent>>>,
    );

    #[async_trait]
    impl crate::native_completion::NativeCompletionPort for RecordingNativeCompletionPort {
        async fn submit(
            &self,
            event: crate::native_completion::NativeCompletionEvent,
        ) -> std::result::Result<(), crate::native_completion::NativeCompletionError> {
            self.0.lock().unwrap().push(event);
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
            recipient: None,
            native_workflow: None,
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
            None,
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
    async fn native_authority_context_overrides_legacy_projection_for_acp_prompt() {
        let mut message = make_msg("perform the assigned bounded work", 10);
        message.native_workflow = Some(crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-81".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "wfrdb475ee4cde59c3d".into(),
            task_id: "task-1".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-81".into(),
            lease_generation: 81,
            expected_revision: 1,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurClaude",
                "dispatch-81",
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        });

        let calls = run_consumer_with_messages(vec![message], 10, 24_000).await;

        assert_eq!(calls.len(), 1);
        let authority = &calls[0].text_blocks[0];
        assert!(authority.contains("NATIVE WORK AUTHORITY"));
        assert!(authority.contains("workflow_run_id: wfrdb475ee4cde59c3d"));
        assert!(authority.contains("role: PRIMARY"));
        assert!(authority.contains("agent: ArthurClaude"));
        assert!(authority.contains("lease_generation: 81"));
        assert!(authority.contains("expected_revision: 1"));
        // Phase 6.4.1F — the rendered authority block now states the
        // explicit precedence hierarchy; the legacy-projection rule
        // lives inside it. Both phrasings must be present so the
        // legacy guarantee is not silently dropped.
        assert!(authority.contains("PRECEDENCE HIERARCHY"));
        assert!(authority.contains("ADVISORY ONLY"));
        assert!(authority.contains("workflow_assignment.json"));
        assert!(authority.contains("non-authoritative projections"));
        assert!(!authority.contains("ArthurCodex"));
        assert_eq!(calls[0].block_count, 3);
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

    // -----------------------------------------------------------------------
    // Native-workflow batching-authority regression tests (NIT 3).
    //
    // Background: `dispatch_batch` derives the whole-batch native authority
    // from `batch.last().native_workflow` (the freshest known state). When
    // `consumer_loop` greedily co-drains native workflow messages with
    // ordinary messages — or with a *different* native fenced dispatch —
    // the wrong metadata wins. The fix gives native workflow messages their
    // own dispatch batch (singleton, no greedy drain) and parks any native
    // message encountered during an ordinary greedy drain into `pending` so
    // the next loop iteration dispatches it independently.
    //
    // These tests pin the contract from the dispatcher's side. The completion
    // hook surface is covered by
    // `native_admission_to_dispatch_to_completion_preserves_authority`.
    // -----------------------------------------------------------------------

    fn make_native_msg(
        prompt: &str,
        tokens: usize,
        metadata: NativeWorkflowMetadataFixture,
    ) -> BufferedMessage {
        let mut msg = make_msg(prompt, tokens);
        msg.native_workflow = Some(crate::admission::NativeWorkflowMetadata {
            dispatch_id: metadata.dispatch_id.into(),
            conversation_key: metadata.conversation_key.into(),
            workflow_run_id: metadata.workflow_run_id.into(),
            task_id: metadata.task_id.into(),
            role: metadata.role.into(),
            agent: metadata.agent.into(),
            lease_id: metadata.lease_id.into(),
            lease_generation: metadata.lease_generation,
            expected_revision: metadata.expected_revision,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                metadata.agent,
                metadata.dispatch_id,
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        });
        msg
    }

    /// Minimal subset of fields used by the regression tests. Avoids leaking
    /// the full struct shape across every callsite.
    struct NativeWorkflowMetadataFixture {
        dispatch_id: &'static str,
        conversation_key: &'static str,
        workflow_run_id: &'static str,
        task_id: &'static str,
        role: &'static str,
        agent: &'static str,
        lease_id: &'static str,
        lease_generation: u64,
        expected_revision: u64,
    }

    fn primary_metadata(
        lease_generation: u64,
        expected_revision: u64,
    ) -> NativeWorkflowMetadataFixture {
        NativeWorkflowMetadataFixture {
            dispatch_id: "dispatch-primary",
            conversation_key: "1540183233654952036",
            workflow_run_id: "wfrun-primary",
            task_id: "task-primary",
            role: "PRIMARY",
            agent: "ArthurClaude",
            lease_id: "lease-primary",
            lease_generation,
            expected_revision,
        }
    }

    fn verifier_metadata(
        lease_generation: u64,
        expected_revision: u64,
    ) -> NativeWorkflowMetadataFixture {
        NativeWorkflowMetadataFixture {
            dispatch_id: "dispatch-verifier",
            conversation_key: "1540183233654952036",
            workflow_run_id: "wfrun-verifier",
            task_id: "task-verifier",
            role: "VERIFIER",
            agent: "ArthurGemini",
            lease_id: "lease-verifier",
            lease_generation,
            expected_revision,
        }
    }

    /// Extract the rendered native authority block (if any) from a recorded
    /// dispatch. Returns `None` for ordinary dispatches.
    fn native_authority_from_call(call: &RecordedDispatch) -> Option<String> {
        // `dispatch_batch` prepends the authority block as the FIRST text
        // block when `native_workflow` is present (line 998). Otherwise the
        // first text block is the optional A13 workflow context (or absent).
        let first = call.text_blocks.first()?;
        if first.contains("NATIVE WORK AUTHORITY") {
            Some(first.clone())
        } else {
            None
        }
    }

    #[tokio::test]
    async fn consumer_native_then_ordinary_splits_into_two_dispatches() {
        // Regression: a native turn followed by an ordinary turn must NOT
        // co-batch — the ordinary message must dispatch in its own batch so
        // the native authority stays bound to its own turn.
        let native = make_native_msg("perform bounded work", 50, primary_metadata(81, 1));
        let ordinary = make_msg("user says hi", 50);

        let calls = run_consumer_with_messages(vec![native, ordinary], 10, 24_000).await;

        assert_eq!(
            calls.len(),
            2,
            "native + ordinary must dispatch as two independent batches"
        );
        // First dispatch owns the native authority (3 blocks: authority +
        // delimiter + prompt).
        assert!(native_authority_from_call(&calls[0]).is_some());
        assert_eq!(calls[0].block_count, 3);
        // Second dispatch is ordinary: no authority block (2 blocks: delimiter
        // + prompt).
        assert!(native_authority_from_call(&calls[1]).is_none());
        assert_eq!(calls[1].block_count, 2);
    }

    #[tokio::test]
    async fn consumer_ordinary_then_native_splits_into_two_dispatches_preserving_fifo() {
        // Regression: when greedy drain of an ordinary batch encounters a
        // native message, the native message must be parked in `pending` and
        // dispatched as its own singleton batch in the next loop iteration.
        // FIFO order must hold: ordinary first, then native.
        let ordinary_a = make_msg("first user message", 50);
        let ordinary_b = make_msg("second user message", 50);
        let native = make_native_msg("perform bounded work", 50, primary_metadata(81, 1));

        let calls =
            run_consumer_with_messages(vec![ordinary_a, ordinary_b, native], 10, 24_000).await;

        assert_eq!(
            calls.len(),
            2,
            "ordinary greedy drain must stop at the native message and dispatch it separately"
        );

        // First dispatch: ordinary batch (greedy drained the first two).
        // 2 arrivals × (delimiter + prompt) = 4 blocks. No native authority.
        assert!(native_authority_from_call(&calls[0]).is_none());
        assert_eq!(calls[0].block_count, 4);

        // Second dispatch: native singleton. Authority block + delimiter +
        // prompt = 3 blocks.
        let authority = native_authority_from_call(&calls[1])
            .expect("second dispatch must carry the native authority");
        assert!(authority.contains("workflow_run_id: wfrun-primary"));
        assert!(authority.contains("role: PRIMARY"));
        assert!(authority.contains("agent: ArthurClaude"));
        assert!(authority.contains("lease_generation: 81"));
        assert_eq!(calls[1].block_count, 3);
    }

    #[tokio::test]
    async fn consumer_consecutive_native_messages_split_into_independent_dispatches() {
        // Regression: two distinct native fenced dispatches (different
        // workflow_run_id / role / agent) must NEVER co-batch. Each must own
        // its own dispatch batch with its own authority.
        let native_a = make_native_msg("bounded work", 50, primary_metadata(81, 1));
        let native_b = make_native_msg("verifier verdict", 50, verifier_metadata(7, 3));

        let calls = run_consumer_with_messages(vec![native_a, native_b], 10, 24_000).await;

        assert_eq!(
            calls.len(),
            2,
            "two native fenced dispatches must split — they are different authorities"
        );

        let authority_a = native_authority_from_call(&calls[0])
            .expect("first dispatch must carry the native authority");
        assert!(authority_a.contains("workflow_run_id: wfrun-primary"));
        assert!(authority_a.contains("role: PRIMARY"));
        assert!(authority_a.contains("agent: ArthurClaude"));
        assert!(authority_a.contains("lease_generation: 81"));
        assert!(!authority_a.contains("ArthurGemini"));
        assert_eq!(calls[0].block_count, 3);

        let authority_b = native_authority_from_call(&calls[1])
            .expect("second dispatch must carry the native authority");
        assert!(authority_b.contains("workflow_run_id: wfrun-verifier"));
        assert!(authority_b.contains("role: VERIFIER"));
        assert!(authority_b.contains("agent: ArthurGemini"));
        assert!(authority_b.contains("lease_generation: 7"));
        assert!(authority_b.contains("expected_revision: 3"));
        assert!(!authority_b.contains("ArthurClaude"));
        assert!(!authority_b.contains("wfrun-primary"));
        assert_eq!(calls[1].block_count, 3);
    }

    #[tokio::test]
    async fn consumer_ordinary_messages_still_greedy_batch() {
        // Regression: the fix must NOT regress ordinary batching. Three
        // ordinary messages still collapse into a single dispatch.
        let calls = run_consumer_with_messages(
            vec![make_msg("a", 50), make_msg("b", 50), make_msg("c", 50)],
            10,
            24_000,
        )
        .await;
        assert_eq!(
            calls.len(),
            1,
            "ordinary → ordinary → ordinary must still greedy-batch into one dispatch"
        );
        assert!(native_authority_from_call(&calls[0]).is_none());
        // 3 arrivals × (delimiter + prompt) = 6 blocks.
        assert_eq!(calls[0].block_count, 6);
    }

    #[tokio::test]
    async fn consumer_dispatch_uses_last_event_origin_event_id_for_merged_batch() {
        let mut first = make_msg("a", 80);
        first.trigger_msg.channel.origin_event_id = Some("evt-first".into());
        let mut second = make_msg("b", 80);
        second.trigger_msg.channel.origin_event_id = Some("evt-second".into());

        let calls = run_consumer_with_messages(vec![first, second], 10, 200).await;
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].dispatch_channel.origin_event_id.as_deref(),
            Some("evt-second")
        );
    }

    #[tokio::test]
    async fn consumer_dispatch_preserves_thread_route_while_refreshing_origin_event_id() {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(1);

        let mut msg = make_msg("hi", 10);
        msg.trigger_msg.channel = ChannelRef {
            platform: "mock".into(),
            channel_id: "parent-channel".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt-fresh".into()),
        };
        tx.send(msg).await.unwrap();
        drop(tx);

        consumer_loop(
            "mock:topic-42".into(),
            ChannelRef {
                platform: "mock".into(),
                channel_id: "topic-42".into(),
                thread_id: Some("topic-42".into()),
                parent_id: Some("parent-channel".into()),
                origin_event_id: Some("evt-stale".into()),
            },
            rx,
            target,
            None,
            adapter,
            10,
            24_000,
            Duration::from_secs(60),
        )
        .await;

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].dispatch_channel.channel_id, "topic-42");
        assert_eq!(
            calls[0].dispatch_channel.thread_id.as_deref(),
            Some("topic-42")
        );
        assert_eq!(
            calls[0].dispatch_channel.parent_id.as_deref(),
            Some("parent-channel")
        );
        assert_eq!(
            calls[0].dispatch_channel.origin_event_id.as_deref(),
            Some("evt-fresh")
        );
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
            None,
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

    #[tokio::test]
    async fn native_workflow_metadata_reaches_completion_port_unchanged() {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-test-123".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "run-test-456".into(),
            task_id: "task-test-789".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-test-123".into(),
            lease_generation: 314,
            expected_revision: 271,
            language: Some("zh-TW".into()),
            project_id: Some("project-test".into()),
            project_root: Some("/project-test".into()),
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurClaude",
                "dispatch-test-123",
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        let hook = crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "done".into(),
            pinned_project_root: None,
            session_key: "discord:1540183233654952036".into(),
            channel: make_channel("thread"),
            agent_identity: None,
            native_workflow: Some(metadata.clone()),
        };

        invoke_workflow_hook_after_dispatch(&target, &hook).await;

        let observed = mock.native_events.lock().unwrap();
        assert_eq!(observed.len(), 1);
        let event = &observed[0];
        assert_eq!(event.dispatch_id, metadata.dispatch_id);
        assert_eq!(event.workflow_run_id, metadata.workflow_run_id);
        assert_eq!(event.task_id, metadata.task_id);
        assert_eq!(event.role, metadata.role);
        assert_eq!(event.agent_identity, metadata.agent);
        assert_eq!(event.lease_id, metadata.lease_id);
        assert_eq!(event.lease_generation, metadata.lease_generation);
        assert_eq!(event.expected_revision, metadata.expected_revision);
        assert_eq!(event.conversation_key, "1540183233654952036");
        assert_ne!(event.conversation_key, hook.session_key);
        assert_eq!(event.language, metadata.language);
        assert_eq!(event.session_id, "discord:1540183233654952036");
        assert_eq!(event.openab_turn_id, "dispatch-test-123");
        assert_eq!(event.outcome, "COMPLETE");
        assert_eq!(event.project_id, "project-test");
        assert_eq!(event.project_root, "/project-test");
        assert!(mock.hook_inputs.lock().unwrap().is_empty());
    }

    /// Phase 6.4.1B — authoritative transport identity flows from the
    /// structured dispatch metadata (`NativeWorkflowMetadata.transport`) into
    /// the `NativeCompletionEvent.transport` field at the production wiring
    /// site (`invoke_workflow_hook_after_dispatch`). The Runtime's
    /// transport-aware conversation identity validator (Phase 6.4.1) reads
    /// this field directly from the HTTP body, so this propagation MUST be
    /// byte-for-byte lossless. Legacy metadata with `transport: None`
    /// produces an event with `transport: None` (Runtime defaults to OPENAB).
    #[tokio::test]
    async fn native_workflow_metadata_transport_propagates_to_completion_event() {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-transport-1".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "run-transport-1".into(),
            task_id: "task-transport-1".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-transport-1".into(),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurClaude",
                "dispatch-transport-1",
            )),
            transport: Some("DISCORD".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        let hook = crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "done".into(),
            pinned_project_root: None,
            session_key: "discord:1540183233654952036".into(),
            channel: make_channel("thread"),
            agent_identity: None,
            native_workflow: Some(metadata.clone()),
        };
        invoke_workflow_hook_after_dispatch(&target, &hook).await;
        let observed = mock.native_events.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].transport.as_deref(),
            Some("DISCORD"),
            "DISCORD transport from metadata must reach NativeCompletionEvent"
        );
    }

    #[tokio::test]
    async fn native_workflow_metadata_without_transport_propagates_none() {
        // Legacy `agent.work` callers that omit the `transport` JSON field
        // deserialize to `transport: None` in NativeWorkflowMetadata; the
        // production site MUST propagate that as `transport: None` into the
        // completion event so Runtime defaults to OPENAB semantics.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-transport-none".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "run-transport-none".into(),
            task_id: "task-transport-none".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-transport-none".into(),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurClaude",
                "dispatch-transport-none",
            )),
            transport: None,
            delivery_destination: None,
            scope_policy: None,
        };
        let hook = crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "done".into(),
            pinned_project_root: None,
            session_key: "discord:1540183233654952036".into(),
            channel: make_channel("thread"),
            agent_identity: None,
            native_workflow: Some(metadata.clone()),
        };
        invoke_workflow_hook_after_dispatch(&target, &hook).await;
        let observed = mock.native_events.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert!(
            observed[0].transport.is_none(),
            "absent metadata.transport must surface as None in completion event"
        );
    }

    #[tokio::test]
    async fn native_reviewer_completion_requires_and_captures_canonical_verdict() {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-verifier".into(),
            conversation_key: "conversation-verifier".into(),
            workflow_run_id: "run-verifier".into(),
            task_id: "task-verifier".into(),
            role: "VERIFIER".into(),
            agent: "ArthurGemini".into(),
            lease_id: "lease-verifier".into(),
            lease_generation: 1,
            expected_revision: 3,
            language: None,
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurGemini",
                "dispatch-verifier",
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        for (text, expected) in [
            ("VERIFIER_PASS", Some("PASS")),
            ("VERIFIER_FAIL", Some("FAIL")),
            ("done", None),
            ("VERIFIER_PASS\nVERIFIER_FAIL", None),
            ("prose containing VERIFIER_PASS is not a verdict", None),
        ] {
            let hook = crate::workflow::service::WorkflowTurnHookInputs {
                terminal: true,
                stop_reason: Some("end_turn".into()),
                raw_assistant_text: text.into(),
                pinned_project_root: None,
                session_key: "session-verifier".into(),
                channel: make_channel("thread"),
                agent_identity: None,
                native_workflow: Some(metadata.clone()),
            };
            invoke_workflow_hook_after_dispatch(&target, &hook).await;
            let mut events = mock.native_events.lock().unwrap();
            match expected {
                Some(outcome) => assert_eq!(events.pop().unwrap().outcome, outcome),
                None => assert!(events.is_empty()),
            }
        }
    }

    /// End-to-end integration: admission → dispatcher → dispatch_batch →
    /// workflow hook → native completion port, with NO hand-constructed
    /// `WorkflowTurnHookInputs`. The hook that drives the completion path
    /// is what `stream_prompt_blocks` returns — exactly the shape the
    /// production ACP adapter would produce. Asserts that the native
    /// authority survives the full trip and resolves to the canonical
    /// `PASS` verdict.
    #[tokio::test]
    async fn native_admission_to_dispatch_to_completion_preserves_authority() {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);

        // `stream_prompt_blocks` will surface this hook after the ACP turn
        // completes. `native_workflow` is left `None` here — the dispatcher
        // fills it from the batch's first (singleton) message before
        // invoking the completion hook.
        mock.set_next_hook(crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "VERIFIER_PASS".into(),
            pinned_project_root: None,
            session_key: String::new(),
            channel: make_channel("T"),
            agent_identity: None,
            native_workflow: None,
        });

        let dispatcher = Arc::new(Dispatcher::with_idle_timeout(
            target,
            10,
            24_000,
            BatchGrouping::Thread,
            Duration::from_secs(60),
        ));

        // Drive a native message through the public submit path so the
        // dispatcher's own consumer_loop / dispatch_batch wiring is what
        // builds the batch — no test-only shortcuts.
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-integration".into(),
            // AAP canonical conversation capabilities are opaque and can be
            // wider than Discord's u64 snowflakes.
            conversation_key: "271837801169159848509375029904518307937".into(),
            workflow_run_id: "wfrun-integration".into(),
            task_id: "task-integration".into(),
            role: "VERIFIER".into(),
            agent: "ArthurGemini".into(),
            lease_id: "lease-integration".into(),
            lease_generation: 314,
            expected_revision: 271,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurGemini",
                "dispatch-integration",
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        let mut message = make_msg("verifier evaluates the patch", 50);
        message.native_workflow = Some(metadata.clone());

        dispatcher
            .submit("mock:T".into(), make_channel("T"), adapter, message)
            .await
            .expect("dispatcher.submit should accept the native message");

        // Wait for the consumer to drain + dispatch_batch + completion hook
        // to fire. 200 ms is comfortably above the test runtime's
        // scheduling jitter for a single-turn flow.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 1. Exactly one dispatch — singleton because the message is native.
        let calls = mock.calls();
        assert_eq!(
            calls.len(),
            1,
            "native message must own its dispatch batch (no greedy drain)"
        );
        assert_eq!(calls[0].block_count, 3);

        // 2. The rendered native authority block reached the prompt.
        let authority = native_authority_from_call(&calls[0])
            .expect("first dispatch must carry the native authority block");
        assert!(authority.contains("NATIVE WORK AUTHORITY"));
        assert!(authority.contains("workflow_run_id: wfrun-integration"));
        assert!(authority.contains("role: VERIFIER"));
        assert!(authority.contains("agent: ArthurGemini"));
        assert!(authority.contains("lease_generation: 314"));
        assert!(authority.contains("expected_revision: 271"));

        // 3. The completion port received the canonical PASS verdict with
        //    authority carried forward verbatim — no manual
        //    `WorkflowTurnHookInputs` construction above the seam.
        let observed = mock.native_events.lock().unwrap();
        assert_eq!(
            observed.len(),
            1,
            "exactly one native completion event must be captured"
        );
        let event = &observed[0];
        assert_eq!(event.dispatch_id, metadata.dispatch_id);
        assert_eq!(event.workflow_run_id, metadata.workflow_run_id);
        assert_eq!(event.task_id, metadata.task_id);
        assert_eq!(event.role, metadata.role);
        assert_eq!(event.agent_identity, metadata.agent);
        assert_eq!(event.lease_id, metadata.lease_id);
        assert_eq!(event.lease_generation, metadata.lease_generation);
        assert_eq!(event.expected_revision, metadata.expected_revision);
        assert_eq!(event.conversation_key, metadata.conversation_key);
        assert_eq!(event.language, metadata.language);
        assert_eq!(event.outcome, "PASS");
    }

    // -----------------------------------------------------------------------
    // Phase 6.2.8: structured native post-turn boundary tracing.
    //
    // These tests assert that the new INFO/WARN observations actually fire
    // with the required correlation fields (`workflow_run_id`,
    // `dispatch_id`, `lease_generation`, `role`) and that they don't leak
    // prompt / raw assistant text / secrets. They use a local
    // `tracing_subscriber` writer so the assertions are isolated from any
    // global subscriber set elsewhere in the test harness.
    // -----------------------------------------------------------------------

    /// Captures tracing output for the duration of an async closure into a
    /// shared `String`. Uses a custom `Write`-implementing writer so the
    /// subscriber can flush through `tracing_subscriber::fmt`. The closure
    /// must return a future so the test can run on the existing tokio
    /// runtime without nesting `block_on`. Returns the captured output
    /// only — future return values are discarded.
    async fn capture_logs<F, Fut>(f: F) -> String
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct CapturingWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for CapturingWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = CapturingWriter(buf.clone());
        // Hold a second handle so we can read the captured bytes after the
        // subscriber (which owns `writer` via the closure passed to
        // `with_writer`) is dropped at the end of this function.
        let reader = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_target(false)
            .finish();
        let _guard = tracing::subscriber::set_default(sub);
        f().await;
        drop(_guard);
        // Copy the captured bytes out under the lock, then release it, so
        // the `MutexGuard` temporary does not outlive `reader`.
        let bytes = reader.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn metadata_for_tracing() -> crate::admission::NativeWorkflowMetadata {
        crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-trace".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "wfrun-trace".into(),
            task_id: "task-trace".into(),
            role: "VERIFIER".into(),
            agent: "ArthurGemini".into(),
            lease_id: "lease-trace".into(),
            lease_generation: 7,
            expected_revision: 3,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurGemini",
                "dispatch-trace",
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        }
    }

    #[tokio::test]
    async fn native_tracing_emits_correlation_fields_on_dispatch_start() {
        // Site 1: `native dispatch turn starting` must fire with all four
        // correlation fields for a native batch, and must NOT log the
        // prompt payload or `raw_assistant_text` (which doesn't exist yet
        // at this point but the rule is forward-applied).
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);

        let mut message = make_msg("perform bounded verifier work", 50);
        message.native_workflow = Some(metadata_for_tracing());

        let logs = capture_logs(|| async move {
            // Drive `dispatch_batch` via the consumer_loop path. We
            // capture logs across the consumer_loop / dispatch_batch
            // boundary, which is the entire window where site 1 fires.
            let mock_inner = target.clone();
            let adapter_inner = adapter.clone();
            // Use a dedicated channel so the consumer exits deterministically.
            let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(4);
            tx.send(message).await.unwrap();
            drop(tx);
            super::consumer_loop(
                "mock:T".into(),
                make_channel("T"),
                rx,
                mock_inner,
                None,
                adapter_inner,
                10,
                24_000,
                Duration::from_secs(60),
            )
            .await;
        })
        .await;

        let _ = mock.calls(); // smoke: dispatcher ran once
        assert!(
            logs.contains("native dispatch turn starting"),
            "site 1 log missing; got: {logs}"
        );
        assert!(logs.contains("workflow_run_id=wfrun-trace"), "{logs}");
        assert!(logs.contains("dispatch_id=dispatch-trace"), "{logs}");
        assert!(logs.contains("lease_generation=7"), "{logs}");
        assert!(logs.contains("role=VERIFIER"), "{logs}");
        // Forbidden payload: prompt text must NOT appear in any tracing line.
        assert!(
            !logs.contains("perform bounded verifier work"),
            "prompt text must not be logged: {logs}"
        );
    }

    #[tokio::test]
    async fn native_tracing_emits_result_shape_ok_hook_with_stop_reason_and_terminal() {
        // Site 2 — `Ok(Some(hook))` branch must log `stop_reason` and
        // `terminal` along with the four correlation fields.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        mock.set_next_hook(crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "VERIFIER_PASS".into(),
            pinned_project_root: None,
            session_key: String::new(),
            channel: make_channel("T"),
            agent_identity: None,
            native_workflow: None,
        });

        let mut message = make_msg("verifier work", 50);
        message.native_workflow = Some(metadata_for_tracing());

        let logs = capture_logs(|| async move {
            let mock_inner = target.clone();
            let adapter_inner = adapter.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(4);
            tx.send(message).await.unwrap();
            drop(tx);
            super::consumer_loop(
                "mock:T".into(),
                make_channel("T"),
                rx,
                mock_inner,
                None,
                adapter_inner,
                10,
                24_000,
                Duration::from_secs(60),
            )
            .await;
        })
        .await;

        assert!(
            logs.contains("native dispatch turn: stream_prompt_blocks returned Ok(Some(hook))"),
            "ok-hook shape log missing: {logs}"
        );
        // `Option<String>` displays as `Some("end_turn")` via the default
        // fmt subscriber; bare `end_turn` would be a regression of the
        // Option wrapper so we keep the substring narrow on purpose.
        assert!(logs.contains("stop_reason=Some(\"end_turn\")"), "{logs}");
        assert!(logs.contains("terminal=true"), "{logs}");
        assert!(logs.contains("shape=\"ok-hook\""), "{logs}");
        assert!(logs.contains("workflow_run_id=wfrun-trace"), "{logs}");
        // Forbidden payload: raw assistant text must NOT appear.
        assert!(
            !logs.contains("VERIFIER_PASS"),
            "raw assistant text must not be logged: {logs}"
        );
    }

    #[tokio::test]
    async fn native_tracing_emits_result_shape_ok_no_hook_branch() {
        // Site 2 — `Ok(None)` branch: stream_prompt_blocks returns no hook.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        // No `set_next_hook` call → stream_prompt_blocks returns Ok(((), None))

        let mut message = make_msg("non-terminal verifier work", 50);
        message.native_workflow = Some(metadata_for_tracing());

        let logs = capture_logs(|| async move {
            let mock_inner = target.clone();
            let adapter_inner = adapter.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(4);
            tx.send(message).await.unwrap();
            drop(tx);
            super::consumer_loop(
                "mock:T".into(),
                make_channel("T"),
                rx,
                mock_inner,
                None,
                adapter_inner,
                10,
                24_000,
                Duration::from_secs(60),
            )
            .await;
        })
        .await;

        assert!(
            logs.contains("native dispatch turn: stream_prompt_blocks returned Ok(None)"),
            "ok-no-hook shape log missing: {logs}"
        );
        assert!(logs.contains("shape=\"ok-no-hook\""), "{logs}");
        assert!(logs.contains("workflow_run_id=wfrun-trace"), "{logs}");
        // When stream_prompt_blocks returns Ok(None), the workflow hook is
        // NOT invoked, so no completion-related logs should appear.
        assert!(
            !logs.contains("entering post-turn boundary"),
            "hook must not enter when Ok(None): {logs}"
        );
        assert!(
            !logs.contains("completion event submitted to port"),
            "no submission when Ok(None): {logs}"
        );
    }

    #[tokio::test]
    async fn native_tracing_emits_result_shape_error_branch() {
        // Site 2 — `Err(error)` branch must WARN with the four correlation
        // fields AND the existing error payload.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        *mock.stream_err.lock().unwrap() = Some("simulated stream failure".into());

        let mut message = make_msg("verifier work", 50);
        message.native_workflow = Some(metadata_for_tracing());

        let logs = capture_logs(|| async move {
            let mock_inner = target.clone();
            let adapter_inner = adapter.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(4);
            tx.send(message).await.unwrap();
            drop(tx);
            super::consumer_loop(
                "mock:T".into(),
                make_channel("T"),
                rx,
                mock_inner,
                None,
                adapter_inner,
                10,
                24_000,
                Duration::from_secs(60),
            )
            .await;
        })
        .await;

        assert!(
            logs.contains("native dispatch turn: stream_prompt_blocks returned Err"),
            "error shape log missing: {logs}"
        );
        assert!(logs.contains("shape=\"error\""), "{logs}");
        // Display of the underlying anyhow error renders as the chain
        // string; we just check the innermost message reached the log.
        assert!(logs.contains("simulated stream failure"), "{logs}");
        assert!(logs.contains("workflow_run_id=wfrun-trace"), "{logs}");
        assert!(logs.contains("role=VERIFIER"), "{logs}");
        // On Err, the workflow hook is not invoked.
        assert!(
            !logs.contains("entering post-turn boundary"),
            "hook must not enter on Err: {logs}"
        );
    }

    #[tokio::test]
    async fn native_tracing_emits_hook_entry_outcome_and_submission_logs() {
        // Sites 3-5: invoke_workflow_hook_after_dispatch must emit
        // entry → outcome-resolution → submission logs with the four
        // correlation fields, in that order. This drives the function
        // directly with a hand-constructed hook (same shape as the
        // integration test) so we can assert each log point.
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let metadata = metadata_for_tracing();
        let hook = crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "VERIFIER_PASS".into(),
            pinned_project_root: None,
            session_key: "session-trace".into(),
            channel: make_channel("T"),
            agent_identity: None,
            native_workflow: Some(metadata.clone()),
        };

        let logs = capture_logs(|| async move {
            let target_inner = target.clone();
            super::invoke_workflow_hook_after_dispatch(&target_inner, &hook).await;
        })
        .await;

        // Site 3.
        assert!(
            logs.contains("native workflow hook: entering post-turn boundary"),
            "site 3 missing: {logs}"
        );
        assert!(logs.contains("stop_reason=Some(\"end_turn\")"), "{logs}");
        assert!(logs.contains("terminal=true"), "{logs}");

        // Site 4.
        assert!(
            logs.contains("native workflow hook: resolved canonical completion outcome"),
            "site 4 missing: {logs}"
        );
        assert!(logs.contains("outcome=PASS"), "{logs}");

        // Site 5.
        assert!(
            logs.contains("native workflow hook: completion event submitted to port"),
            "site 5 missing: {logs}"
        );
        assert!(logs.contains("outcome=PASS"), "{logs}");

        // Forbidden payloads.
        assert!(
            !logs.contains("VERIFIER_PASS"),
            "raw assistant text must not appear in logs: {logs}"
        );

        // Ordering: site 3 must come before site 4, site 4 before site 5.
        let pos_entry = logs.find("entering post-turn boundary").unwrap();
        let pos_outcome = logs.find("resolved canonical completion outcome").unwrap();
        let pos_submit = logs.find("completion event submitted to port").unwrap();
        assert!(pos_entry < pos_outcome, "ordering broken: {logs}");
        assert!(pos_outcome < pos_submit, "ordering broken: {logs}");
    }

    #[tokio::test]
    async fn native_tracing_emits_existing_failure_log_when_port_submit_errors() {
        // Site 6: when `native_completion_port.submit` returns Err, the
        // existing failure message ("native completion callback failed")
        // must still fire — verbatim — and now carry the four correlation
        // fields + `outcome`. We simulate this by routing
        // `invoke_workflow_hook_after_dispatch` through a one-off target
        // (`FailingCompletionPortTarget`) whose port always errors. The
        // production `MockDispatchTarget` cannot swap its port, so this
        // target is scoped to this single failure-path test.
        let failing_port: Arc<dyn crate::native_completion::NativeCompletionPort> =
            Arc::new(FailingNativeCompletionPort);
        let failing_target: Arc<dyn DispatchTarget> =
            Arc::new(FailingCompletionPortTarget::new(failing_port.clone()));
        let metadata = metadata_for_tracing();
        let hook = crate::workflow::service::WorkflowTurnHookInputs {
            terminal: true,
            stop_reason: Some("end_turn".into()),
            raw_assistant_text: "VERIFIER_PASS".into(),
            pinned_project_root: None,
            session_key: "session-trace".into(),
            channel: make_channel("T"),
            agent_identity: None,
            native_workflow: Some(metadata.clone()),
        };

        let logs = capture_logs(|| async move {
            let target_inner = failing_target.clone();
            super::invoke_workflow_hook_after_dispatch(&target_inner, &hook).await;
        })
        .await;

        // The existing failure log line must remain verbatim.
        assert!(
            logs.contains("native completion callback failed"),
            "existing failure log line must be preserved verbatim: {logs}"
        );
        // It must now carry the four correlation fields + outcome.
        assert!(logs.contains("workflow_run_id=wfrun-trace"), "{logs}");
        assert!(logs.contains("dispatch_id=dispatch-trace"), "{logs}");
        assert!(logs.contains("lease_generation=7"), "{logs}");
        assert!(logs.contains("role=VERIFIER"), "{logs}");
        assert!(logs.contains("outcome=PASS"), "{logs}");
        // No submission log on failure.
        assert!(
            !logs.contains("completion event submitted to port"),
            "must not log submission success when port errors: {logs}"
        );
        // Site 4 (outcome resolved) still fires — failure is at port
        // submission, not at outcome resolution.
        assert!(
            logs.contains("resolved canonical completion outcome"),
            "outcome resolution log must still fire before port submit: {logs}"
        );
    }

    /// Always-failing `NativeCompletionPort` for the failure-log regression
    /// test. Returns a stable error so the dispatcher path can be
    /// asserted without depending on error-message wording.
    struct FailingNativeCompletionPort;

    #[async_trait]
    impl crate::native_completion::NativeCompletionPort for FailingNativeCompletionPort {
        async fn submit(
            &self,
            _event: crate::native_completion::NativeCompletionEvent,
        ) -> std::result::Result<(), crate::native_completion::NativeCompletionError> {
            Err(crate::native_completion::NativeCompletionError::Transport(
                "simulated port failure".into(),
            ))
        }
    }

    /// Minimal `DispatchTarget` that exposes a configurable failing
    /// completion port. Every other method is a no-op so the hook path
    /// never errors before reaching `native_completion_port().submit()`.
    struct FailingCompletionPortTarget {
        port: Arc<dyn crate::native_completion::NativeCompletionPort>,
        tech_lead_user_ids: std::collections::HashSet<u64>,
    }

    impl FailingCompletionPortTarget {
        fn new(port: Arc<dyn crate::native_completion::NativeCompletionPort>) -> Self {
            Self {
                port,
                tech_lead_user_ids: std::collections::HashSet::new(),
            }
        }
    }

    #[async_trait]
    impl DispatchTarget for FailingCompletionPortTarget {
        fn reactions_config(&self) -> &ReactionsConfig {
            // Leak a default — tests are short-lived; this is fine for the
            // failure-path test that never inspects reactions.
            use std::sync::OnceLock;
            static CFG: OnceLock<ReactionsConfig> = OnceLock::new();
            CFG.get_or_init(ReactionsConfig::default)
        }
        fn workspace_aliases(&self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }
        fn bot_home(&self) -> std::path::PathBuf {
            std::path::PathBuf::from("/tmp")
        }
        async fn ensure_session(
            &self,
            _session_key: &str,
            _project: Option<&ProjectContext>,
            _write_policy: Option<&str>,
        ) -> Result<bool> {
            Ok(true)
        }
        async fn reset_session(&self, _session_key: &str) {}
        async fn pinned_project_root(&self, _session_key: &str) -> Option<std::path::PathBuf> {
            None
        }
        fn tech_lead_user_ids(&self) -> std::collections::HashSet<u64> {
            self.tech_lead_user_ids.clone()
        }
        async fn stream_prompt_blocks(
            &self,
            _adapter: &Arc<dyn ChatAdapter>,
            _session_key: &str,
            _content_blocks: Vec<ContentBlock>,
            _thread_channel: &ChannelRef,
            _reactions: Arc<StatusReactionController>,
            _other_bot_present: bool,
            _recipient: Option<(String, String)>,
        ) -> Result<((), Option<crate::workflow::service::WorkflowTurnHookInputs>)> {
            Ok(((), None))
        }
        fn workflow_service(&self) -> Option<Arc<crate::workflow::service::WorkflowService>> {
            None
        }
        fn native_completion_port(&self) -> crate::native_completion::SharedNativeCompletionPort {
            self.port.clone()
        }
        fn autonomous_ingress_client(
            &self,
        ) -> Option<Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>> {
            None
        }
        fn autonomous_ingress_config(&self) -> Option<&crate::config::AutonomousIngressConfig> {
            None
        }
        fn autonomous_ingress_agent_identity(&self) -> Option<&str> {
            None
        }
        async fn observe_workflow_turn_hook(
            &self,
            _hook: &crate::workflow::service::WorkflowTurnHookInputs,
        ) {
        }
    }

    // ===================================================================
    // Phase 6.2.9 FIX ROUND 4 — production-path session-key wiring tests
    //
    // Production smoke at 2026-08-29 08:52:33 CST proved that
    // `dispatch_batch` was routing native-work dispatches into
    // `SessionPool.get_or_create` with the legacy Discord
    // `discord:1539923659345502208` key, causing historical turn
    // replay. The fix lives at `dispatch_batch` line ~879, which must
    // prefer `batch.last().native_workflow.native_execution_session_key`
    // over `thread_channel.session_pool_key()` when one is present.
    //
    // Prior tests mocked `DispatchTarget::ensure_session` to IGNORE the
    // session_key parameter — that is exactly why the bug slipped
    // through. These tests exercise the same public production path
    // (`consumer_loop` → `dispatch_batch` → `ensure_session`) and
    // assert the actual key value the SessionPool would receive.
    // ===================================================================

    /// Build a BufferedMessage carrying a native-work payload for tests.
    /// Mirrors the production shape produced by
    /// `src/ctl.rs::RuntimeHandler::handle_agent_work` (lines 1325-1364).
    /// Distinct from the existing helper `make_native_msg(prompt, tokens,
    /// NativeWorkflowMetadataFixture)` because this variant overrides the
    /// `trigger_msg.channel` to the configured Discord delivery target,
    /// which is exactly the leak surface the Phase 6.2.9 FIX ROUND 4 tests
    /// must exercise.
    fn make_native_msg_targeted(
        prompt: &str,
        agent: &str,
        dispatch_id: &str,
        delivery_channel_id: &str,
    ) -> BufferedMessage {
        let mut msg = make_msg(prompt, 10);
        msg.native_workflow = Some(crate::admission::NativeWorkflowMetadata {
            dispatch_id: dispatch_id.into(),
            conversation_key: "1539923659345502208".into(),
            workflow_run_id: format!("wf-{}", dispatch_id),
            task_id: format!("task-{}", dispatch_id),
            role: "PRIMARY".into(),
            agent: agent.into(),
            lease_id: format!("lease-{}", dispatch_id),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                agent,
                dispatch_id,
            )),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        });
        msg.trigger_msg.channel = make_channel(delivery_channel_id);
        msg
    }

    /// Variant of `run_consumer_with_messages` that returns both the
    /// recorded `stream_prompt_blocks` calls and the captured
    /// `ensure_session` keys, so tests can assert the actual key the
    /// SessionPool receives.
    async fn run_consumer_returning_session_keys(
        msgs: Vec<BufferedMessage>,
    ) -> (Vec<RecordedDispatch>, Vec<String>, Arc<MockDispatchTarget>) {
        let mock = Arc::new(MockDispatchTarget::new());
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(msgs.len().max(1));
        for m in msgs {
            tx.send(m).await.expect("send into pre-loaded mpsc");
        }
        drop(tx);

        consumer_loop(
            "mock:T".into(),
            make_channel("T"),
            rx,
            target,
            None,
            adapter,
            10,
            24_000,
            Duration::from_secs(60),
        )
        .await;

        (mock.calls(), mock.session_keys(), mock)
    }

    #[tokio::test]
    async fn native_agent_work_uses_dispatch_execution_session_key() {
        // Phase 6.2.9 invariant: `set agent.work` produces an execution
        // key of `native-dispatch:ArthurClaude:test-dispatch-123` and
        // the SessionPool MUST see exactly that key.
        let msgs = vec![make_native_msg_targeted(
            "perform the bounded work",
            "ArthurClaude",
            "test-dispatch-123",
            "1539923659345502208",
        )];
        let (calls, keys, _mock) = run_consumer_returning_session_keys(msgs).await;

        assert_eq!(calls.len(), 1, "expected exactly one dispatch");
        assert_eq!(keys.len(), 1, "expected exactly one ensure_session call");
        assert_eq!(
            keys[0], "native-dispatch:ArthurClaude:test-dispatch-123",
            "SessionPool must receive the canonical native-dispatch key, \
             not a Discord delivery key"
        );
        assert_eq!(
            calls[0].session_key,
            "native-dispatch:ArthurClaude:test-dispatch-123"
        );
    }

    #[tokio::test]
    async fn native_delivery_channel_not_used_as_acp_session_key() {
        // The Discord delivery channel id (1539923659345502208) MUST
        // remain transport-only metadata. It must NEVER be reused as the
        // ACP session-pool key on the native-work path.
        let msgs = vec![make_native_msg_targeted(
            "perform the bounded work",
            "ArthurClaude",
            "dispatch-no-leak",
            "1539923659345502208",
        )];
        let (_calls, keys, _mock) = run_consumer_returning_session_keys(msgs).await;

        assert_eq!(keys.len(), 1);
        let key = &keys[0];
        assert!(
            !key.starts_with("discord:"),
            "native-work dispatch leaked the Discord delivery channel into the pool key: {key}"
        );
        assert!(
            !key.contains("1539923659345502208"),
            "native-work dispatch leaked the literal delivery channel id into the pool key: {key}"
        );
        assert!(
            key.starts_with("native-dispatch:"),
            "expected fenced native-dispatch prefix, got: {key}"
        );
    }

    #[tokio::test]
    async fn native_dispatch_ids_produce_distinct_execution_keys() {
        // Two `set agent.work` admissions with two distinct
        // `dispatch_id`s MUST produce two distinct execution-session
        // keys, otherwise the pool would treat them as the same ACP
        // session and the second dispatch would replay the first's
        // historical turns.
        let msg_a = make_native_msg_targeted(
            "work A",
            "ArthurClaude",
            "dispatch-A-001",
            "1539923659345502208",
        );
        let msg_b = make_native_msg_targeted(
            "work B",
            "ArthurClaude",
            "dispatch-B-002",
            "1539923659345502208",
        );
        // Run them in sequence in two consumer invocations to mirror
        // the scheduler's independent admission path.
        let (_calls_a, keys_a, _mock_a) = run_consumer_returning_session_keys(vec![msg_a]).await;
        let (_calls_b, keys_b, _mock_b) = run_consumer_returning_session_keys(vec![msg_b]).await;

        assert_eq!(keys_a.len(), 1);
        assert_eq!(keys_b.len(), 1);
        assert_eq!(
            keys_a[0], "native-dispatch:ArthurClaude:dispatch-A-001",
            "first dispatch produced wrong key"
        );
        assert_eq!(
            keys_b[0], "native-dispatch:ArthurClaude:dispatch-B-002",
            "second dispatch produced wrong key"
        );
        assert_ne!(
            keys_a[0], keys_b[0],
            "two distinct dispatch_ids MUST produce two distinct execution keys"
        );
    }

    #[tokio::test]
    async fn human_discord_still_uses_legacy_session_key() {
        // Regression guard for the human (non-native) path: a message
        // with `native_workflow = None` MUST keep using the legacy
        // `<platform>:<thread_id>` pool key derived from
        // `ChannelRef::session_pool_key()`. The Phase 6.2.9 fix MUST
        // NOT collateral-damage human conversational continuity —
        // both Discord and the `mock:` test platform fall under the
        // same branch.
        //
        // We deliberately assert the *shape* (no `native-dispatch:`
        // prefix, derived from the channel ref) rather than a hard-
        // coded `discord:` literal, because this test reuses the
        // mock-channel helper which uses `platform = "mock"`. The
        // production invariant the fix preserves is "non-native turns
        // do not get the native-dispatch key shape".
        let human = make_msg("hi bot, how are you?", 10);
        // Sanity: the test fixture above leaves `native_workflow = None`
        // and the channel is the mock `T` thread from `make_msg`.
        assert!(human.native_workflow.is_none());

        let (_calls, keys, _mock) = run_consumer_returning_session_keys(vec![human]).await;

        assert_eq!(keys.len(), 1);
        let key = &keys[0];
        assert!(
            !key.starts_with("native-dispatch:"),
            "human (non-native) turn leaked the native-dispatch prefix: {key}"
        );
        assert!(
            !key.contains("native-dispatch:"),
            "human (non-native) turn leaked a native-dispatch key fragment: {key}"
        );
        assert_eq!(
            key, "mock:T",
            "human (non-native) turn must derive its pool key from the \
             legacy `<platform>:<thread_id>` ChannelRef.session_pool_key() shape \
             (the mock helper uses platform=`mock` and thread_id=`T`)"
        );
    }

    // ===================================================================
    // Phase 6.4 — OpenAB → AAP autonomous ingress dispatch tests
    //
    // These tests cover the spec-required behaviors:
    //   1. human_autonomous_request_routes_to_aap_before_acp
    //   2. accepted_aap_workflow_does_not_double_execute_human_turn
    //   3. autonomous_request_aap_unavailable_does_not_fallback_to_acp
    //   4. autonomous_request_auth_failure_does_not_fallback_to_acp
    //   5. ordinary_human_chat_still_uses_acp
    //   6. retry / duplicate Discord message does not generate
    //      duplicate autonomous entry submissions
    //   7. bot-authored messages do not create unrelated workflows
    //   8. non-Tech-Lead unauthorized human message obeys existing
    //      authorization policy
    //
    // All tests construct a MockDispatchTarget with an injected
    // AutonomousIngressClient + config + agent identity override. The
    // dispatch loop's Phase 6.4 branch lives at the seam right after
    // the A13 gate admit; these tests verify the consumer-level
    // invariant — when AAP accepts, no ordinary ACP dispatch is
    // recorded; when AAP fails, no ordinary ACP dispatch is recorded
    // either (fail-closed).
    // ===================================================================

    fn phase64_config(agents: &[&str], universal: bool) -> crate::config::AutonomousIngressConfig {
        crate::config::AutonomousIngressConfig {
            aap_agents: agents.iter().map(|s| s.to_string()).collect(),
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "TEST_TOKEN_ENV".into(),
            project_id: "arthur-ai-platform".into(),
            request_timeout_seconds: 5,
            aap_universal_humans: universal,
        }
    }

    fn tech_lead_sender_json(user_id: u64) -> String {
        format!(
            r#"{{"schema":"openab.sender.v1","sender_id":"{user_id}","sender_name":"tech_lead","is_bot":false,"display_name":"tech_lead"}}"#
        )
    }

    fn ordinary_human_sender_json(user_id: u64) -> String {
        format!(
            r#"{{"schema":"openab.sender.v1","sender_id":"{user_id}","sender_name":"human","is_bot":false,"display_name":"human"}}"#
        )
    }

    fn bot_sender_json() -> String {
        r#"{"schema":"openab.sender.v1","sender_id":"9999","sender_name":"bot","is_bot":true,"display_name":"bot"}"#.to_string()
    }

    fn make_msg_with_sender(prompt: &str, tokens: usize, sender_json: String) -> BufferedMessage {
        BufferedMessage {
            sender_json,
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
            recipient: None,
            native_workflow: None,
        }
    }

    async fn run_phase64(
        msg: BufferedMessage,
        client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient>,
        config: crate::config::AutonomousIngressConfig,
        agent: &str,
        tech_lead_ids: std::collections::HashSet<u64>,
    ) -> Vec<RecordedDispatch> {
        let mock = Arc::new(
            MockDispatchTarget::new()
                .with_autonomous_ingress(client, config, agent)
                .with_tech_lead_user_ids(tech_lead_ids),
        );
        let target: Arc<dyn DispatchTarget> = mock.clone();
        let adapter: Arc<dyn ChatAdapter> = Arc::new(MockChatAdapter);
        let (tx, rx) = tokio::sync::mpsc::channel::<BufferedMessage>(1);
        tx.send(msg).await.unwrap();
        drop(tx);
        consumer_loop(
            "mock:T".into(),
            make_channel("T"),
            rx,
            target,
            None,
            adapter,
            1,
            100,
            std::time::Duration::from_secs(5),
        )
        .await;
        mock.calls()
    }

    #[tokio::test]
    async fn phase64_human_autonomous_request_routes_to_aap_before_acp() {
        // Spec scenario 1: a Tech-Lead-authorized human message arrives,
        // no workflow_assignment.json exists, the daemon is declared
        // AAP-autonomous. AAP MUST be consulted and ordinary ACP MUST
        // NOT receive the dispatch.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let msg = make_msg_with_sender(
            "請做一個極小且可逆的 repository smoke test",
            20,
            tech_lead_sender_json(tech_lead_id),
        );
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_accept();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            calls.is_empty(),
            "ordinary ACP must not run when AAP autonomous ingress accepts the human turn"
        );
        assert_eq!(
            fake.call_count(),
            1,
            "AAP client must be invoked exactly once"
        );
    }

    #[tokio::test]
    async fn phase64_accepted_aap_workflow_does_not_double_execute_human_turn() {
        // Spec scenario 2: AAP accepts. The same human turn MUST NOT
        // also flow into ordinary ACP. This is the message-consumption
        // invariant.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let msg = make_msg_with_sender("fix it", 10, tech_lead_sender_json(tech_lead_id));
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_accept();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            calls.is_empty(),
            "AAP acceptance must consume the human turn — no ordinary ACP"
        );
        assert_eq!(
            fake.call_count(),
            1,
            "AAP client must be invoked exactly once"
        );
    }

    #[tokio::test]
    async fn phase64_aap_unavailable_does_not_fallback_to_acp() {
        // Spec scenario 3: AAP unreachable. The dispatch MUST fail
        // closed without falling back to ordinary ACP.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let msg = make_msg_with_sender("fix it", 10, tech_lead_sender_json(tech_lead_id));
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_unreachable();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            calls.is_empty(),
            "AAP unavailable must NOT fall back to ordinary ACP"
        );
        assert_eq!(
            fake.call_count(),
            1,
            "AAP client must be invoked exactly once"
        );
    }

    #[tokio::test]
    async fn phase64_auth_failure_does_not_fallback_to_acp() {
        // Spec scenario 4: AAP auth missing. The dispatch MUST fail
        // closed without falling back to ordinary ACP. Auth failures
        // are not retryable.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let msg = make_msg_with_sender("fix it", 10, tech_lead_sender_json(tech_lead_id));
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_auth_missing();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            calls.is_empty(),
            "AAP auth failure must NOT fall back to ordinary ACP"
        );
        assert_eq!(fake.call_count(), 1);
    }

    #[tokio::test]
    async fn phase64_ordinary_human_chat_still_uses_acp() {
        // Spec scenario 5: an ordinary human chat message (NOT routed
        // as autonomous) must still reach ordinary ACP. This proves
        // legacy behavior is preserved when the routing contract does
        // not select the message.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        // Sender is human but NOT Tech Lead. Tech-lead check fails →
        // ordinary ACP proceeds.
        let non_tech_lead: u64 = 1234567890;
        let msg = make_msg_with_sender(
            "這段程式碼在做什麼？",
            10,
            ordinary_human_sender_json(non_tech_lead),
        );
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_accept();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            !calls.is_empty(),
            "ordinary human chat (non-Tech-Lead) must still flow to ordinary ACP"
        );
        assert_eq!(
            fake.call_count(),
            0,
            "AAP client must NOT be consulted for non-Tech-Lead human chat"
        );
    }

    #[tokio::test]
    async fn phase64_bot_messages_do_not_create_workflows() {
        // Spec scenario 7: bot-authored messages must not create
        // workflows. They flow into ordinary ACP unchanged.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let msg = make_msg_with_sender("bot echo", 10, bot_sender_json());
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_accept();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            !calls.is_empty(),
            "bot-authored message must flow into ordinary ACP unchanged"
        );
        assert_eq!(
            fake.call_count(),
            0,
            "AAP client must NOT be invoked for bot-authored traffic"
        );
    }

    #[tokio::test]
    async fn phase64_unauthorized_human_message_obeys_policy() {
        // Spec scenario 8: a non-Tech-Lead human message addressed to
        // a declared AAP agent must obey the existing authorization
        // policy — it falls through to ordinary ACP, not AAP. The
        // Phase 6.4 contract does NOT widen authorization; it only
        // adds a deterministic routing layer when policy says yes.
        std::env::set_var("ARTHUR_AGENT_NAME", "ArthurClaude");
        let tech_lead_id: u64 = 645496545805991947;
        let mut tech_lead_ids = std::collections::HashSet::new();
        tech_lead_ids.insert(tech_lead_id);
        let unauthorized_human: u64 = 11111111;
        let msg = make_msg_with_sender(
            "請做 Phase 6.4",
            10,
            ordinary_human_sender_json(unauthorized_human),
        );
        let fake = crate::autonomous_ingress::FakeAutonomousIngressClient::always_accept();
        let client: Arc<dyn crate::autonomous_ingress::AutonomousIngressClient> = fake.clone();
        let calls = run_phase64(
            msg,
            client,
            phase64_config(&["ArthurClaude"], false),
            "ArthurClaude",
            tech_lead_ids,
        )
        .await;
        assert!(
            !calls.is_empty(),
            "non-Tech-Lead human must flow to ordinary ACP, never to AAP"
        );
        assert_eq!(fake.call_count(), 0);
    }
}

//! Serving side of the control-plane client: turn one `cp/delegate` into one
//! `cp/delegate_result`.
//!
//! ## Invariants
//!
//! - **Admission never executes.** An over-cap or duplicate delegation is
//!   answered with `status = failed` and an explanation, without touching the
//!   session pool. The CP already fast-fails on its own accounting; this is
//!   the runtime's own last word on its capacity, and it must be cheap.
//! - **One fresh session per delegation.** The session key is derived from
//!   `(instance_id, delegation_id, admission)`, so no delegation can observe
//!   another's conversation, and a re-admission of the same reusable id cannot
//!   resume an earlier admission's session. The session is discarded on every
//!   terminal outcome — nothing accumulates in the pool.
//! - **Exactly one result per admitted delegation.** Every path through
//!   [`DelegationExecutor::serve`] returns a `DelegateResultParams`; the
//!   client is what decides whether it can still be sent (on a dead socket it
//!   cannot, and the CP synthesizes `target_disconnected` instead).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use openab_cp::proto::{AdmissionToken, DelegateForward, DelegateResultParams, DelegationStatus};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tracing::{info, warn};

/// Session-pool key for one ADMISSION of one delegation.
///
/// Hashed rather than concatenated so an operator-visible key can never carry
/// a `delegation_id` chosen to collide with a chat thread key (they share one
/// namespace in the pool) and so its length is bounded regardless of what the
/// initiator sent. `instance_id` distinguishes replicas of the same logical
/// agent. The CP admission token is mixed in so a re-admission of the same
/// reusable id can never resume an earlier admission's session — in
/// particular one orphaned by a drain-timeout abort, whose transcript and
/// tool state would otherwise leak into the new run through the pool's
/// get-or-create semantics.
pub fn delegation_session_key(
    instance_id: &str,
    delegation_id: &str,
    admission: AdmissionToken,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instance_id.as_bytes());
    hasher.update(delegation_id.as_bytes());
    hasher.update(admission.to_be_bytes());
    format!("control-plane:{:x}", hasher.finalize())
}

/// Outcome of one delegated prompt as reported by a [`PromptRunner`].
#[derive(Debug, Clone, Default)]
pub struct PromptOutcome {
    /// Reply text to hand back to the initiator.
    pub text: String,
    /// Agent/broker-level error that ended the turn.
    pub error: Option<String>,
    /// The turn produced nothing and reported zero output tokens — a
    /// provider/model/auth failure that must not be reported as success.
    pub silent_failure: bool,
}

/// The prompt-execution seam.
///
/// Production is [`RouterPromptRunner`] (ACP session pool + `AdapterRouter`).
/// It exists as a trait so the client's state machine can be exercised against
/// the real CP server without a coding agent on the box: the integration test
/// injects a runner that answers from a script.
#[async_trait]
pub trait PromptRunner: Send + Sync + 'static {
    /// Run the delegated prompt to completion in `session_key`.
    async fn run(&self, session_key: &str, forward: &DelegateForward) -> Result<PromptOutcome>;

    /// Best-effort interrupt of the in-flight turn for `session_key`.
    async fn cancel(&self, session_key: &str);

    /// Drop `session_key` and its bookkeeping.
    async fn discard(&self, session_key: &str);
}

/// Local admission + execution of delegations for one runtime instance.
pub struct DelegationExecutor {
    runner: Arc<dyn PromptRunner>,
    instance_id: String,
    /// Ceiling from the registration ack (the CP may clamp what we advertised).
    /// Updated on every re-register, hence atomic rather than a constructor arg.
    effective_max: AtomicU32,
    /// Per-turn hard ceiling, from `[pool].prompt_hard_timeout_secs`. The
    /// delegation deadline is the other clock; the shorter one wins.
    prompt_hard_timeout: Duration,
    /// Admitted delegations → their cancel signal. Also the capacity counter:
    /// its length is the number of active delegated sessions reported in
    /// `cp/heartbeat`.
    inflight: Mutex<BTreeMap<String, (AdmissionToken, Arc<Notify>)>>,
}

/// Why admission refused, as the message sent back in `status = failed`.
#[derive(Debug)]
enum Refusal {
    OverCapacity { active: u32, max: u32 },
    Duplicate,
}

impl Refusal {
    fn message(&self) -> String {
        match self {
            Refusal::OverCapacity { active, max } => format!(
                "runtime is at its local delegation capacity ({active}/{max}); \
                 the delegation was not started"
            ),
            Refusal::Duplicate => "delegation_id is already in flight on this runtime; \
                 the delegation was not started"
                .to_string(),
        }
    }
}

impl DelegationExecutor {
    pub fn new(
        runner: Arc<dyn PromptRunner>,
        instance_id: impl Into<String>,
        effective_max: u32,
        prompt_hard_timeout: Duration,
    ) -> Self {
        Self {
            runner,
            instance_id: instance_id.into(),
            effective_max: AtomicU32::new(effective_max),
            prompt_hard_timeout,
            inflight: Mutex::new(BTreeMap::new()),
        }
    }

    /// Adopt the budget the CP acked. Called on every (re-)registration.
    pub fn set_effective_max(&self, max: u32) {
        self.effective_max.store(max, Ordering::Relaxed);
    }

    pub fn effective_max(&self) -> u32 {
        self.effective_max.load(Ordering::Relaxed)
    }

    /// Number of admitted, not-yet-finished delegations.
    pub fn active(&self) -> u32 {
        self.inflight.lock().expect("inflight mutex").len() as u32
    }

    /// Reserve a slot for `delegation_id`, or explain why not.
    fn admit(
        &self,
        delegation_id: &str,
        admission: AdmissionToken,
    ) -> std::result::Result<Arc<Notify>, Refusal> {
        let max = self.effective_max();
        let mut g = self.inflight.lock().expect("inflight mutex");
        if g.contains_key(delegation_id) {
            return Err(Refusal::Duplicate);
        }
        let active = g.len() as u32;
        if active >= max {
            return Err(Refusal::OverCapacity { active, max });
        }
        let signal = Arc::new(Notify::new());
        g.insert(delegation_id.to_string(), (admission, Arc::clone(&signal)));
        Ok(signal)
    }

    fn release(&self, delegation_id: &str) {
        self.inflight
            .lock()
            .expect("inflight mutex")
            .remove(delegation_id);
    }

    /// Signal cancellation for one delegation (`cp/cancel` from the CP).
    /// Returns `false` when the id is not in flight here — the CP's view can
    /// legitimately be ahead of ours (it also cancels on deadline).
    ///
    /// `notify_one` rather than `notify_waiters`: it leaves a permit behind, so
    /// a cancel that arrives between admission and the first poll of the
    /// serving task is still observed instead of being lost.
    pub fn cancel(&self, delegation_id: &str, admission: AdmissionToken) -> bool {
        let signal = {
            let g = self.inflight.lock().expect("inflight mutex");
            match g.get(delegation_id) {
                // The token names ONE admission of this reusable id. A stale
                // cancel — the CP swept admission A, this worker was already
                // re-serving B under the same id — must not abort B: that is
                // the worker-side half of the misdelivery the wire token
                // exists to close.
                Some((adm, s)) if *adm == admission => Some(Arc::clone(s)),
                Some((adm, _)) => {
                    tracing::info!(
                        delegation_id,
                        live = *adm,
                        stale = admission,
                        "cp/cancel names a superseded admission — ignoring"
                    );
                    None
                }
                None => None,
            }
        };
        match signal {
            Some(s) => {
                s.notify_one();
                true
            }
            None => false,
        }
    }

    /// Signal cancellation for every in-flight delegation: connection loss and
    /// shutdown. Each task cleans its session up and returns a `Cancelled`
    /// result the caller is free to drop — on a dead socket the CP synthesizes
    /// `target_disconnected` for the initiator, so sending ours is neither
    /// possible nor needed.
    pub fn cancel_all(&self) {
        let signals: Vec<Arc<Notify>> = self
            .inflight
            .lock()
            .expect("inflight mutex")
            .values()
            .map(|(_, s)| Arc::clone(s))
            .collect();
        for s in signals {
            s.notify_one();
        }
    }

    /// Admit, run, and classify one forwarded delegation.
    ///
    /// Always resolves to a result frame payload — the refusal paths included,
    /// so the initiator is never left waiting on its deadline for a runtime
    /// that had already decided not to run.
    pub async fn serve(self: Arc<Self>, forward: DelegateForward) -> DelegateResultParams {
        let id = forward.delegation_id.clone();
        let cancel = match self.admit(&id, forward.admission) {
            Ok(signal) => signal,
            Err(refusal) => {
                let error = refusal.message();
                warn!(delegation_id = %id, from = %forward.from, %error, "delegation refused");
                return failed(&id, forward.admission, error);
            }
        };
        // RAII: the slot must free even if this task is ABORTED mid-await —
        // the client aborts serving tasks that outlive the drain window on
        // disconnect, and a plain post-await release would be skipped there,
        // leaking the inflight entry forever (with the default cap of 1, the
        // worker would refuse every delegation after reconnecting).
        let _slot = SlotGuard {
            executor: self.as_ref(),
            id: id.clone(),
        };
        self.execute(&forward, cancel).await
    }

    async fn execute(
        &self,
        forward: &DelegateForward,
        cancel: Arc<Notify>,
    ) -> DelegateResultParams {
        let id = &forward.delegation_id;
        let session_key = delegation_session_key(&self.instance_id, id, forward.admission);

        // Two clocks bound the turn: the CP-enforced delegation deadline and
        // the runtime's own per-turn ceiling. Take the nearer one — an already
        // elapsed deadline means there is nothing worth starting.
        let Ok(remaining) = (forward.deadline - chrono::Utc::now()).to_std() else {
            warn!(delegation_id = %id, deadline = %forward.deadline, "delegation arrived past its deadline");
            return timed_out(id, forward.admission);
        };
        let budget = remaining.min(self.prompt_hard_timeout);
        info!(
            delegation_id = %id,
            from = %forward.from,
            chain_depth = forward.chain.len(),
            budget_secs = budget.as_secs(),
            "serving delegation"
        );

        // `notified()` is created BEFORE the run so a cancel racing the first
        // poll is not missed: `cancel` leaves a permit (`notify_one`), and this
        // future consumes it whenever it is first polled.
        let cancelled = cancel.notified();
        let outcome = tokio::select! {
            biased;
            _ = cancelled => {
                info!(delegation_id = %id, "delegation cancelled");
                self.cancel_and_discard(&session_key).await;
                return DelegateResultParams {
                    delegation_id: id.clone(),
                    admission: forward.admission,
                    status: DelegationStatus::Cancelled,
                    result: None,
                    error: None,
                };
            }
            run = tokio::time::timeout(budget, self.runner.run(&session_key, forward)) => run,
        };

        match outcome {
            Err(_elapsed) => {
                warn!(delegation_id = %id, budget_secs = budget.as_secs(), "delegation exceeded its local deadline");
                self.cancel_and_discard(&session_key).await;
                timed_out(id, forward.admission)
            }
            Ok(Err(e)) => {
                // The turn could not be driven at all (no session, dead agent).
                let error = format!("{e:#}");
                warn!(delegation_id = %id, %error, "delegation failed before completion");
                self.bounded_discard(&session_key).await;
                failed(id, forward.admission, error)
            }
            Ok(Ok(outcome)) => {
                if let Some(error) = outcome.error {
                    self.bounded_discard(&session_key).await;
                    warn!(delegation_id = %id, %error, "delegation ended in an agent error");
                    return failed(id, forward.admission, error);
                }
                if outcome.silent_failure {
                    self.bounded_discard(&session_key).await;
                    warn!(delegation_id = %id, "delegation produced an empty turn (silent failure)");
                    return failed(
                        id,
                        forward.admission,
                        "agent returned an empty turn (0 output tokens) — \
                         likely a provider/model/auth failure",
                    );
                }
                // Turn completed cleanly. Spawn discard off the critical path
                // so a slow or wedged discard does not delay result delivery to
                // the initiator (F53). With admission-scoped session keys, a
                // deferred discard is safe: a subsequent admission of the same
                // delegation id gets a distinct session key.
                self.spawn_discard(session_key);
                info!(delegation_id = %id, bytes = outcome.text.len(), "delegation completed");
                DelegateResultParams {
                    delegation_id: id.clone(),
                    admission: forward.admission,
                    status: DelegationStatus::Completed,
                    result: Some(cap_result(outcome.text)),
                    error: None,
                }
            }
        }
    }

    /// Best-effort, BOUNDED session teardown: `session/cancel` writes to the
    /// agent's stdin, which can wedge (dead child, full pipe), and the pool's
    /// discard takes its write lock, which can be starved. Both share ONE
    /// deadline so a wedged teardown cannot burn the client's disconnect drain window (F52).
    async fn cancel_and_discard(&self, session_key: &str) {
        let deadline = tokio::time::Instant::now() + TEARDOWN_BOUND;
        let _ = tokio::time::timeout_at(deadline, self.runner.cancel(session_key)).await;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!(
                session_key,
                "session cancel consumed the teardown bound; leaving discard to pool cleanup"
            );
            return;
        }
        if tokio::time::timeout_at(deadline, self.runner.discard(session_key))
            .await
            .is_err()
        {
            warn!(session_key, "session discard exceeded its bound; leaving it to pool cleanup");
        }
    }

    /// Discard with the same bound as cancel; on overrun the session is left
    /// to the pool's own idle/hung cleanup rather than blocking this task.
    async fn bounded_discard(&self, session_key: &str) {
        if tokio::time::timeout(TEARDOWN_BOUND, self.runner.discard(session_key))
            .await
            .is_err()
        {
            warn!(session_key, "session discard exceeded its bound; leaving it to pool cleanup");
        }
    }

    /// Spawn discard off the critical path on success (F53).
    fn spawn_discard(&self, session_key: String) {
        let runner = Arc::clone(&self.runner);
        tokio::spawn(async move {
            if tokio::time::timeout(TEARDOWN_BOUND, runner.discard(&session_key))
                .await
                .is_err()
            {
                warn!(
                    session_key = %session_key,
                    "session discard exceeded its bound; leaving it to pool cleanup"
                );
            }
        });
    }
}

/// Bound on each session-teardown step (cancel, discard). Matches the pool's
/// own cleanup bound and stays under the client's disconnect drain window.
const TEARDOWN_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// Client-side ceiling on a `cp/delegate_result` body.
///
/// The CP enforces `max_frame_bytes` (default 1 MiB) at the WS transport,
/// BEFORE parsing — its own `max_result_bytes` truncation therefore can never
/// save an oversized frame: the transport drops the connection, and every
/// in-flight delegation on this worker dies as `target_disconnected`. Capping
/// here keeps the frame safely under the transport limit; the CP still applies
/// its (typically smaller) `max_result_bytes` on what arrives.
const MAX_RESULT_BYTES: usize = 512 * 1024;

/// Ceiling on a `cp/delegate_result` error string. Errors ride the same
/// transport frame as results but are diagnostics, not payloads, so the
/// budget is far tighter. Without this, an unbounded `anyhow` chain or an
/// agent-authored error would hit the CP's pre-parse `max_frame_bytes` and
/// drop the connection — the exact failure `MAX_RESULT_BYTES` closes for the
/// success path.
const MAX_ERROR_BYTES: usize = 64 * 1024;

/// Returns the byte length of `s` when serialized inside a JSON string literal.
///
/// In JSON (RFC 8259), `"` and `\` become 2 bytes (`\"`, `\\`), control
/// characters 0x00..=0x1f become 2 bytes (`\b`, `\t`, `\n`, `\f`, `\r`) or
/// 6 bytes (`\u00xx`), and all other UTF-8 bytes pass through unchanged (1 byte
/// each).
pub(crate) fn json_escaped_len(s: &str) -> usize {
    s.bytes()
        .map(|b| match b {
            b'\"' | b'\\' => 2,
            b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
            c if c < 0x20 => 6,
            _ => 1,
        })
        .sum()
}

fn cap_text(text: String, budget: usize) -> String {
    if json_escaped_len(&text) <= budget {
        return text;
    }
    let marker = format!(
        "\n...[truncated by worker: {} bytes total exceeded the transport budget]",
        text.len()
    );
    let marker_escaped_len = json_escaped_len(&marker);
    let keep_escaped = budget.saturating_sub(marker_escaped_len);
    let mut cut = 0;
    let mut current_escaped = 0;
    for (idx, ch) in text.char_indices() {
        let ch_escaped = match ch {
            '\"' | '\\' => 2,
            '\x08' | '\t' | '\n' | '\x0c' | '\r' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        };
        if current_escaped + ch_escaped > keep_escaped {
            break;
        }
        current_escaped += ch_escaped;
        cut = idx + ch.len_utf8();
    }
    let mut out = String::with_capacity(cut + marker.len());
    out.push_str(&text[..cut]);
    out.push_str(&marker);
    out
}

fn cap_result(text: String) -> String {
    cap_text(text, MAX_RESULT_BYTES)
}

/// Frees a delegation's inflight slot on drop — including the drop that
/// happens when the serving task is aborted at an await point.
struct SlotGuard<'a> {
    executor: &'a DelegationExecutor,
    id: String,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.executor.release(&self.id);
    }
}

fn failed(
    delegation_id: &str,
    admission: AdmissionToken,
    error: impl Into<String>,
) -> DelegateResultParams {
    DelegateResultParams {
        delegation_id: delegation_id.to_string(),
        // Echoed verbatim from the forward: the CP correlates terminal frames
        // per admission, so a late result for a superseded admission of this
        // id is dropped instead of completing the wrong delegation.
        admission,
        status: DelegationStatus::Failed,
        result: None,
        // Capped HERE, not at call sites: every error source (anyhow chains,
        // agent-authored errors) must share the transport-safe bound, and a
        // new call site must not be able to forget it.
        error: Some(cap_text(error.into(), MAX_ERROR_BYTES)),
    }
}

fn timed_out(delegation_id: &str, admission: AdmissionToken) -> DelegateResultParams {
    DelegateResultParams {
        delegation_id: delegation_id.to_string(),
        admission,
        status: DelegationStatus::Timeout,
        result: None,
        error: Some("delegation deadline elapsed at the serving runtime".into()),
    }
}

// ---------------------------------------------------------------------------
// Production runner: ACP session pool + AdapterRouter
// ---------------------------------------------------------------------------

use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use crate::reactions::StatusReactionController;

/// Platform label for delegated turns. Not a chat platform: it exists so
/// session keys, logs, and the router's platform switches can tell a
/// delegation apart from a user conversation.
pub const CP_PLATFORM: &str = "control-plane";

/// [`PromptRunner`] over the real ACP session pool.
///
/// Reuses `AdapterRouter::stream_prompt_blocks` — the same turn driver every
/// chat platform uses, so tool events, liveness checks, the hard timeout, and
/// silent-failure classification behave identically here — with a sink adapter
/// standing in for the platform.
pub struct RouterPromptRunner {
    router: Arc<AdapterRouter>,
}

impl RouterPromptRunner {
    pub fn new(router: Arc<AdapterRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl PromptRunner for RouterPromptRunner {
    async fn run(&self, session_key: &str, forward: &DelegateForward) -> Result<PromptOutcome> {
        // A delegation is always a fresh session, so this creates one rather
        // than resuming; `working_dir` stays the configured default.
        self.router.pool().get_or_create(session_key, None).await?;

        let adapter: Arc<dyn ChatAdapter> = Arc::new(SinkAdapter);
        let channel = ChannelRef {
            platform: CP_PLATFORM.to_string(),
            channel_id: forward.delegation_id.clone(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };
        // Reactions are constructed disabled: there is no message to react to.
        let reactions = Arc::new(StatusReactionController::new(
            false,
            Arc::clone(&adapter),
            MessageRef {
                channel: channel.clone(),
                message_id: String::new(),
            },
            crate::config::ReactionEmojis::default(),
            crate::config::ReactionTiming::default(),
        ));

        let blocks = AdapterRouter::pack_arrival_event(
            &delegation_context_json(forward),
            &forward.prompt,
            Vec::new(),
        );
        let execution = self
            .router
            .stream_prompt_blocks(
                &adapter,
                session_key,
                blocks,
                &channel,
                reactions,
                false, // other_bot_present: no channel, no other bots
                None,  // no native-streaming recipient
            )
            .await?;

        Ok(PromptOutcome {
            text: execution.final_text,
            error: execution.terminal_error,
            silent_failure: execution.silent_failure,
        })
    }

    async fn cancel(&self, session_key: &str) {
        if let Err(e) = self.router.pool().cancel_session(session_key).await {
            // Nothing in flight to cancel is the common benign case.
            tracing::debug!(error = %e, "cancel_session on a delegated session");
        }
    }

    async fn discard(&self, session_key: &str) {
        self.router.pool().discard_session(session_key).await;
    }
}

/// The arrival metadata block for a delegated turn.
///
/// Carried inside the same `<sender_context>` envelope every platform arrival
/// uses (so agents keep one place to look for provenance) but with its own
/// schema: the fields that matter here are the CP-authenticated ones — who
/// asked, through which ancestry, and by when — and `chain`/`deadline` have no
/// counterpart in `openab.sender.v1`. Every value is stamped by the CP, so the
/// agent may trust it.
fn delegation_context_json(forward: &DelegateForward) -> String {
    serde_json::json!({
        "schema": "openab.delegation.v1",
        "delegation_id": forward.delegation_id,
        "from": forward.from,
        "chain": forward.chain,
        "deadline": forward.deadline.to_rfc3339(),
    })
    .to_string()
}

/// A `ChatAdapter` that delivers nowhere.
///
/// A delegation's reply travels back over the control-plane socket as
/// `cp/delegate_result`, not to a channel, so every write is dropped and the
/// text is read from the returned `PromptExecution` instead. Forcing
/// send-once (`use_streaming = false`) is what makes that safe: no
/// placeholder is posted, no edit loop is spawned, and the full turn text is
/// composed exactly once at the end.
struct SinkAdapter;

#[async_trait]
impl ChatAdapter for SinkAdapter {
    fn platform(&self) -> &'static str {
        CP_PLATFORM
    }

    /// No chunking: the delegation result is one payload, and the CP applies
    /// its own `max_result_bytes` cap.
    fn message_limit(&self) -> usize {
        usize::MAX
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false
    }

    async fn send_message(&self, channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: String::new(),
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

    async fn edit_message(&self, _msg: &MessageRef, _content: &str) -> Result<()> {
        Ok(())
    }

    async fn delete_message(&self, _msg: &MessageRef) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn forward(id: &str, secs: i64) -> DelegateForward {
        DelegateForward {
            delegation_id: id.into(),
            admission: 1,
            prompt: "do the thing".into(),
            deadline: chrono::Utc::now() + chrono::Duration::seconds(secs),
            from: "prod/koudu".into(),
            chain: vec!["prod/koudu".into()],
        }
    }

    /// Scripted runner: records lifecycle calls and answers as configured.
    #[derive(Default)]
    struct FakeRunner {
        /// Reply text on success.
        text: String,
        /// If set, `run` fails with this message.
        run_error: Option<String>,
        /// If set, the outcome carries this agent error.
        agent_error: Option<String>,
        silent_failure: bool,
        /// If set, `run` sleeps this long before answering.
        delay: Option<Duration>,
        /// If set, `cancel` sleeps this long before answering.
        cancel_delay: Option<Duration>,
        /// If set, `discard` sleeps this long before answering.
        discard_delay: Option<Duration>,
        started: AtomicUsize,
        cancelled: Mutex<Vec<String>>,
        discarded: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn completing(text: &str) -> Arc<Self> {
            Arc::new(Self {
                text: text.into(),
                ..Default::default()
            })
        }
        fn starts(&self) -> usize {
            self.started.load(Ordering::Relaxed)
        }
        fn discarded(&self) -> Vec<String> {
            self.discarded.lock().unwrap().clone()
        }
        async fn wait_discarded(&self, expected_len: usize) -> Vec<String> {
            for _ in 0..100 {
                let d = self.discarded();
                if d.len() >= expected_len {
                    return d;
                }
                tokio::task::yield_now().await;
            }
            self.discarded()
        }
        fn cancelled(&self) -> Vec<String> {
            self.cancelled.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PromptRunner for FakeRunner {
        async fn run(
            &self,
            _session_key: &str,
            _forward: &DelegateForward,
        ) -> Result<PromptOutcome> {
            self.started.fetch_add(1, Ordering::Relaxed);
            if let Some(d) = self.delay {
                tokio::time::sleep(d).await;
            }
            if let Some(ref e) = self.run_error {
                return Err(anyhow::anyhow!(e.clone()));
            }
            Ok(PromptOutcome {
                text: self.text.clone(),
                error: self.agent_error.clone(),
                silent_failure: self.silent_failure,
            })
        }

        async fn cancel(&self, session_key: &str) {
            if let Some(d) = self.cancel_delay {
                tokio::time::sleep(d).await;
            }
            self.cancelled.lock().unwrap().push(session_key.to_string());
        }

        async fn discard(&self, session_key: &str) {
            if let Some(d) = self.discard_delay {
                tokio::time::sleep(d).await;
            }
            self.discarded.lock().unwrap().push(session_key.to_string());
        }
    }

    fn executor(runner: Arc<FakeRunner>, max: u32) -> Arc<DelegationExecutor> {
        Arc::new(DelegationExecutor::new(
            runner,
            "i-test",
            max,
            Duration::from_secs(600),
        ))
    }

    #[tokio::test]
    async fn success_maps_to_completed_and_discards_the_session() {
        let runner = FakeRunner::completing("here you go");
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-1", 60)).await;
        assert_eq!(res.status, DelegationStatus::Completed);
        assert_eq!(res.result.as_deref(), Some("here you go"));
        assert!(res.error.is_none());
        assert_eq!(
            runner.wait_discarded(1).await,
            vec![delegation_session_key("i-test", "d-1", 1)],
            "a fresh-per-delegation session must not survive its delegation"
        );
        assert_eq!(ex.active(), 0, "the slot is released");
    }

    #[tokio::test]
    async fn over_capacity_is_refused_without_executing() {
        let runner = FakeRunner::completing("ok");
        let ex = executor(Arc::clone(&runner), 1);
        // Occupy the only slot: `admit` is what reserves capacity, so the
        // entry stands until the (never-spawned) serving task releases it.
        let _held = ex.admit("d-held", 1).expect("first slot");
        let res = Arc::clone(&ex).serve(forward("d-2", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert!(res.error.unwrap().contains("local delegation capacity"));
        assert_eq!(runner.starts(), 0, "a refused delegation never runs");
        assert!(runner.discarded().is_empty(), "and touches no session");
    }

    #[tokio::test]
    async fn duplicate_delegation_id_is_refused_without_executing() {
        let runner = FakeRunner::completing("ok");
        let ex = executor(Arc::clone(&runner), 4);
        let _held = ex.admit("d-3", 1).expect("slot");
        let res = Arc::clone(&ex).serve(forward("d-3", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert!(res.error.unwrap().contains("already in flight"));
        assert_eq!(runner.starts(), 0);
    }

    #[tokio::test]
    async fn effective_max_from_the_ack_is_what_bounds_admission() {
        let runner = FakeRunner::completing("ok");
        let ex = executor(Arc::clone(&runner), 4);
        ex.set_effective_max(1); // CP clamped us
        let _held = ex.admit("d-a", 1).expect("slot");
        let res = Arc::clone(&ex).serve(forward("d-b", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert!(
            res.error.unwrap().contains("(1/1)"),
            "the clamped ceiling, not the advertised one, is enforced"
        );
        assert_eq!(runner.starts(), 0);
    }

    #[tokio::test]
    async fn agent_error_maps_to_failed() {
        let runner = Arc::new(FakeRunner {
            agent_error: Some("provider returned HTTP 500".into()),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-4", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert_eq!(res.error.as_deref(), Some("provider returned HTTP 500"));
        assert_eq!(runner.discarded().len(), 1);
    }

    #[tokio::test]
    async fn silent_failure_maps_to_failed_not_completed() {
        let runner = Arc::new(FakeRunner {
            silent_failure: true,
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-5", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert!(res.error.unwrap().contains("empty turn"));
    }

    #[tokio::test]
    async fn broker_error_maps_to_failed() {
        let runner = Arc::new(FakeRunner {
            run_error: Some("no connection for session".into()),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-6", 60)).await;
        assert_eq!(res.status, DelegationStatus::Failed);
        assert!(res.error.unwrap().contains("no connection"));
        assert_eq!(
            runner.discarded().len(),
            1,
            "the session is still cleaned up"
        );
    }

    #[tokio::test]
    async fn a_past_deadline_times_out_without_executing() {
        let runner = FakeRunner::completing("ok");
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-7", -1)).await;
        assert_eq!(res.status, DelegationStatus::Timeout);
        assert_eq!(runner.starts(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_local_deadline_times_out_and_cleans_the_session_up() {
        let runner = Arc::new(FakeRunner {
            delay: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let res = Arc::clone(&ex).serve(forward("d-8", 5)).await;
        assert_eq!(res.status, DelegationStatus::Timeout);
        assert_eq!(runner.starts(), 1, "it did start");
        let key = delegation_session_key("i-test", "d-8", 1);
        assert_eq!(runner.cancelled(), vec![key.clone()]);
        assert_eq!(runner.discarded(), vec![key]);
        assert_eq!(ex.active(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_mid_flight_maps_to_cancelled_and_drops_the_session() {
        let runner = Arc::new(FakeRunner {
            delay: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let serving = tokio::spawn({
            let ex = Arc::clone(&ex);
            async move { ex.serve(forward("d-9", 600)).await }
        });
        // Let the task admit and start before cancelling.
        while ex.active() == 0 {
            tokio::task::yield_now().await;
        }
        assert!(ex.cancel("d-9", 1), "the id is in flight");
        let res = serving.await.unwrap();
        assert_eq!(res.status, DelegationStatus::Cancelled);
        assert!(res.result.is_none());
        let key = delegation_session_key("i-test", "d-9", 1);
        assert_eq!(runner.cancelled(), vec![key.clone()]);
        assert_eq!(runner.discarded(), vec![key]);
        assert_eq!(ex.active(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_all_ends_every_in_flight_delegation() {
        let runner = Arc::new(FakeRunner {
            delay: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 4);
        let mut tasks = Vec::new();
        for id in ["d-x", "d-y"] {
            let ex = Arc::clone(&ex);
            tasks.push(tokio::spawn(
                async move { ex.serve(forward(id, 600)).await },
            ));
        }
        while ex.active() < 2 {
            tokio::task::yield_now().await;
        }
        ex.cancel_all();
        for t in tasks {
            assert_eq!(t.await.unwrap().status, DelegationStatus::Cancelled);
        }
        assert_eq!(ex.active(), 0);
        assert_eq!(runner.discarded().len(), 2);
    }

    #[test]
    fn cancel_of_an_unknown_id_is_a_no_op() {
        let ex = executor(FakeRunner::completing("ok"), 1);
        assert!(!ex.cancel("never-seen", 1));
    }

    #[test]
    fn session_keys_are_namespaced_bounded_and_instance_scoped() {
        let a = delegation_session_key("i-1", "d-1", 1);
        let b = delegation_session_key("i-2", "d-1", 1);
        assert!(a.starts_with("control-plane:"));
        assert_ne!(a, b, "another replica's session never collides");
        assert_eq!(a.len(), "control-plane:".len() + 64);
        // A re-admission of the same reusable id gets a fresh session: an
        // orphaned session from an aborted admission can never be resumed.
        let c = delegation_session_key("i-1", "d-1", 2);
        assert_ne!(a, c, "a re-admission of the same id gets a fresh session");
        // A hostile id cannot forge another platform's key shape.
        let hostile = delegation_session_key("i-1", "discord:12345", 1);
        assert!(hostile.starts_with("control-plane:"));
        assert_eq!(hostile.len(), a.len());
    }

    #[test]
    fn the_delegation_context_block_carries_the_cp_stamped_provenance() {
        let v: serde_json::Value =
            serde_json::from_str(&delegation_context_json(&forward("d-10", 30))).unwrap();
        assert_eq!(v["schema"], "openab.delegation.v1");
        assert_eq!(v["delegation_id"], "d-10");
        assert_eq!(v["from"], "prod/koudu");
        assert_eq!(v["chain"][0], "prod/koudu");
        assert!(v["deadline"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn the_sink_adapter_never_streams() {
        // Streaming would post a placeholder to a channel that does not exist
        // and split the reply the executor has to return whole.
        assert!(!SinkAdapter.use_streaming(false));
        assert!(!SinkAdapter.use_streaming(true));
        assert!(!SinkAdapter.uses_native_streaming(false));
        assert!(!SinkAdapter.uses_assistant_status());
    }

    #[test]
    fn oversized_results_are_capped_below_the_transport_limit() {
        // An uncapped result larger than the CP's max_frame_bytes (1 MiB)
        // would be dropped at the WS transport before the CP's own
        // max_result_bytes truncation could run, killing the connection and
        // every in-flight delegation with it.
        let big = "x".repeat(2 * 1024 * 1024);
        let capped = cap_result(big);
        assert!(capped.len() <= MAX_RESULT_BYTES);
        assert!(capped.ends_with("bytes total exceeded the transport budget]"));

        // Multibyte char straddling the cut must not split a boundary.
        let emoji = "\u{1F980}".repeat(MAX_RESULT_BYTES / 4 + 64);
        let capped = cap_result(emoji);
        assert!(capped.len() <= MAX_RESULT_BYTES);
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());

        // Under the cap: untouched.
        assert_eq!(cap_result("small".into()), "small");
    }

    #[test]
    fn oversized_errors_are_capped_below_the_transport_limit() {
        // The error field rides the same frame as the result and hits the
        // same pre-parse max_frame_bytes ceiling at the CP. failed() must
        // bound every error source (anyhow chains, agent-authored errors),
        // no matter the call site.
        let big = "e".repeat(2 * 1024 * 1024);
        let res = failed("d-err", 7, big);
        let err = res.error.expect("failed() always carries an error");
        assert!(err.len() <= MAX_ERROR_BYTES);
        assert!(err.ends_with("bytes total exceeded the transport budget]"));
        assert_eq!(res.admission, 7, "the admission echo survives the cap");
        assert_eq!(res.status, DelegationStatus::Failed);

        // Multibyte char straddling the cut must not split a boundary.
        let emoji = "\u{1F980}".repeat(MAX_ERROR_BYTES / 4 + 64);
        let res = failed("d-err", 7, emoji);
        let err = res.error.expect("failed() always carries an error");
        assert!(err.len() <= MAX_ERROR_BYTES);
        assert!(std::str::from_utf8(err.as_bytes()).is_ok());

        // Under the cap: untouched.
        let res = failed("d-err", 7, "short");
        assert_eq!(res.error.as_deref(), Some("short"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_aborted_serving_task_still_frees_its_slot() {
        // The client aborts serving tasks that outlive the drain window on
        // disconnect. A plain post-await release would be skipped by the
        // abort, leaking the inflight entry: with the default cap of 1 the
        // worker would then refuse every delegation after reconnecting.
        let runner = Arc::new(FakeRunner {
            delay: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let task = {
            let ex = Arc::clone(&ex);
            tokio::spawn(async move { ex.serve(forward("d-abort", 600)).await })
        };
        while ex.active() < 1 {
            tokio::task::yield_now().await;
        }

        task.abort();
        let _ = task.await; // JoinError::Cancelled — the abort landed

        assert_eq!(ex.active(), 0, "abort must free the slot via the guard");
        // And the freed slot is genuinely reusable.
        let done = FakeRunner::completing("ok");
        let ex2 = executor(done, 1);
        let r = ex2.serve(forward("d-after", 600)).await;
        assert_eq!(r.status, DelegationStatus::Completed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stale_cancel_does_not_abort_a_reserving_admission() {
        // Worker-side half of the wire-token contract: the CP swept admission
        // A of "d-1" and its best-effort cancel (stamped with A's token) can
        // arrive after this worker started serving re-admission B. The cancel
        // must not abort B.
        let runner = Arc::new(FakeRunner {
            delay: Some(Duration::from_secs(300)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let mut fwd = forward("d-1", 600);
        fwd.admission = 42; // B's admission
        let task = {
            let ex = Arc::clone(&ex);
            tokio::spawn(async move { ex.serve(fwd).await })
        };
        while ex.active() < 1 {
            tokio::task::yield_now().await;
        }

        // A's stale cancel: same id, older token.
        assert!(!ex.cancel("d-1", 7), "a stale token must be ignored");
        assert_eq!(ex.active(), 1, "B keeps running");

        // B's own cancel works.
        assert!(ex.cancel("d-1", 42));
        let result = task.await.unwrap();
        assert_eq!(result.status, DelegationStatus::Cancelled);
        assert_eq!(result.admission, 42, "the terminal frame names B");
    }

    #[test]
    fn json_escaped_len_matches_serde_json() {
        let test_cases = vec![
            "",
            "hello world",
            "escapes: \x08 \t \n \x0c \r",
            "control: \x00 \x1b \x1f",
            "quotes and slashes: \" \\ \"",
            "multibyte: 🦀 日本語 \u{1F980}",
        ];
        for case in test_cases {
            let expected = serde_json::to_string(case).unwrap().len() - 2;
            assert_eq!(
                json_escaped_len(case),
                expected,
                "mismatch for case: {:?}",
                case
            );
        }
    }

    #[test]
    fn escape_heavy_results_cannot_inflate_past_the_transport_budget() {
        // 500 KiB of ESC characters (\x1b) has raw len <= 512 KiB, but JSON
        // serializes each as \u001b (6 bytes), inflating to ~3 MiB.
        // Unchecked, this exceeds the CP's pre-parse max_frame_bytes (1 MiB),
        // causing the CP to close the WebSocket and kill every co-inflight
        // delegation (F46).
        let escapes = "\x1b".repeat(500 * 1024);
        let capped = cap_result(escapes);
        let serialized = serde_json::to_string(&capped).unwrap();
        // The serialized JSON string (including quotes) must remain within
        // the transport budget (MAX_RESULT_BYTES + 2 for quotes).
        assert!(
            serialized.len() <= MAX_RESULT_BYTES + 2,
            "serialized length {} exceeds budget",
            serialized.len()
        );
        assert!(capped.ends_with("bytes total exceeded the transport budget]"));

        // Strings with quotes and backslashes also inflate (2x) and must be bounded.
        let slashes = "\\\"".repeat(300 * 1024);
        let capped_slashes = cap_result(slashes);
        let serialized_slashes = serde_json::to_string(&capped_slashes).unwrap();
        assert!(
            serialized_slashes.len() <= MAX_RESULT_BYTES + 2,
            "serialized slashes length {} exceeds budget",
            serialized_slashes.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_cancel_and_stalled_discard_share_one_teardown_bound() {
        // Both cancel and discard can wedge (dead agent process, starved pool lock).
        // cancel_and_discard must bound the SUM of both to TEARDOWN_BOUND (5s),
        // not 5s each (~10s total), which would exceed the client's DRAIN_TIMEOUT (5s) (F52).
        let runner = Arc::new(FakeRunner {
            cancel_delay: Some(Duration::from_secs(10)),
            discard_delay: Some(Duration::from_secs(10)),
            ..Default::default()
        });
        let ex = executor(Arc::clone(&runner), 1);
        let start = tokio::time::Instant::now();
        ex.cancel_and_discard("s-wedged").await;
        let elapsed = start.elapsed();
        assert_eq!(
            elapsed, TEARDOWN_BOUND,
            "teardown must complete at the shared 5s bound"
        );
        assert!(
            runner.discarded().is_empty(),
            "discard must be skipped when cancel consumes the entire bound"
        );
    }
}

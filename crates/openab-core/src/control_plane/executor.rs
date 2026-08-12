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
//!   `(instance_id, delegation_id)`, so no delegation can observe another's
//!   conversation, and a replayed id after a reconnect cannot resume a stale
//!   one. The session is discarded on every terminal outcome — nothing
//!   accumulates in the pool.
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
use openab_cp::proto::{DelegateForward, DelegateResultParams, DelegationStatus};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tracing::{info, warn};

/// Session-pool key for one delegation.
///
/// Hashed rather than concatenated so an operator-visible key can never carry
/// a `delegation_id` chosen to collide with a chat thread key (they share one
/// namespace in the pool) and so its length is bounded regardless of what the
/// initiator sent. `instance_id` is mixed in so the same id seen after a
/// reconnect maps to a different session.
pub fn delegation_session_key(instance_id: &str, delegation_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instance_id.as_bytes());
    hasher.update(delegation_id.as_bytes());
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
    inflight: Mutex<BTreeMap<String, Arc<Notify>>>,
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
    fn admit(&self, delegation_id: &str) -> std::result::Result<Arc<Notify>, Refusal> {
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
        g.insert(delegation_id.to_string(), Arc::clone(&signal));
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
    pub fn cancel(&self, delegation_id: &str) -> bool {
        let signal = self
            .inflight
            .lock()
            .expect("inflight mutex")
            .get(delegation_id)
            .map(Arc::clone);
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
            .map(Arc::clone)
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
        let cancel = match self.admit(&id) {
            Ok(signal) => signal,
            Err(refusal) => {
                let error = refusal.message();
                warn!(delegation_id = %id, from = %forward.from, %error, "delegation refused");
                return failed(&id, error);
            }
        };
        let outcome = self.execute(&forward, cancel).await;
        self.release(&id);
        outcome
    }

    async fn execute(
        &self,
        forward: &DelegateForward,
        cancel: Arc<Notify>,
    ) -> DelegateResultParams {
        let id = &forward.delegation_id;
        let session_key = delegation_session_key(&self.instance_id, id);

        // Two clocks bound the turn: the CP-enforced delegation deadline and
        // the runtime's own per-turn ceiling. Take the nearer one — an already
        // elapsed deadline means there is nothing worth starting.
        let Ok(remaining) = (forward.deadline - chrono::Utc::now()).to_std() else {
            warn!(delegation_id = %id, deadline = %forward.deadline, "delegation arrived past its deadline");
            return timed_out(id);
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
                // Best-effort, BOUNDED: session/cancel writes to the agent's
                // stdin, which can wedge (dead child, full pipe). The pool's
                // own cleanup uses the same 5s bound. Discard must run either
                // way or the slot leaks.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.runner.cancel(&session_key),
                )
                .await;
                self.runner.discard(&session_key).await;
                return DelegateResultParams {
                    delegation_id: id.clone(),
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
                // Best-effort, BOUNDED: session/cancel writes to the agent's
                // stdin, which can wedge (dead child, full pipe). The pool's
                // own cleanup uses the same 5s bound. Discard must run either
                // way or the slot leaks.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.runner.cancel(&session_key),
                )
                .await;
                self.runner.discard(&session_key).await;
                timed_out(id)
            }
            Ok(Err(e)) => {
                // The turn could not be driven at all (no session, dead agent).
                let error = format!("{e:#}");
                warn!(delegation_id = %id, %error, "delegation failed before completion");
                self.runner.discard(&session_key).await;
                failed(id, error)
            }
            Ok(Ok(outcome)) => {
                self.runner.discard(&session_key).await;
                if let Some(error) = outcome.error {
                    warn!(delegation_id = %id, %error, "delegation ended in an agent error");
                    return failed(id, error);
                }
                if outcome.silent_failure {
                    warn!(delegation_id = %id, "delegation produced an empty turn (silent failure)");
                    return failed(
                        id,
                        "agent returned an empty turn (0 output tokens) — \
                         likely a provider/model/auth failure",
                    );
                }
                info!(delegation_id = %id, bytes = outcome.text.len(), "delegation completed");
                DelegateResultParams {
                    delegation_id: id.clone(),
                    status: DelegationStatus::Completed,
                    result: Some(outcome.text),
                    error: None,
                }
            }
        }
    }
}

fn failed(delegation_id: &str, error: impl Into<String>) -> DelegateResultParams {
    DelegateResultParams {
        delegation_id: delegation_id.to_string(),
        status: DelegationStatus::Failed,
        result: None,
        error: Some(error.into()),
    }
}

fn timed_out(delegation_id: &str) -> DelegateResultParams {
    DelegateResultParams {
        delegation_id: delegation_id.to_string(),
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
            self.cancelled.lock().unwrap().push(session_key.to_string());
        }

        async fn discard(&self, session_key: &str) {
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
            runner.discarded(),
            vec![delegation_session_key("i-test", "d-1")],
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
        let _held = ex.admit("d-held").expect("first slot");
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
        let _held = ex.admit("d-3").expect("slot");
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
        let _held = ex.admit("d-a").expect("slot");
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
        let key = delegation_session_key("i-test", "d-8");
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
        assert!(ex.cancel("d-9"), "the id is in flight");
        let res = serving.await.unwrap();
        assert_eq!(res.status, DelegationStatus::Cancelled);
        assert!(res.result.is_none());
        let key = delegation_session_key("i-test", "d-9");
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
        assert!(!ex.cancel("never-seen"));
    }

    #[test]
    fn session_keys_are_namespaced_bounded_and_instance_scoped() {
        let a = delegation_session_key("i-1", "d-1");
        let b = delegation_session_key("i-2", "d-1");
        assert!(a.starts_with("control-plane:"));
        assert_ne!(a, b, "a replayed id after reconnect gets a fresh session");
        assert_eq!(a.len(), "control-plane:".len() + 64);
        // A hostile id cannot forge another platform's key shape.
        let hostile = delegation_session_key("i-1", "discord:12345");
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
}

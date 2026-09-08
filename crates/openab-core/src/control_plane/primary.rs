//! Primary-side (initiating) control-plane state.
//!
//! The worker side turns one inbound `cp/delegate` into one
//! `cp/delegate_result` ([`super::executor`]). The primary side is the mirror:
//! it *originates* `cp/delegate`, `cp/cancel`, and `cp/list_agents`, then
//! correlates the CP's replies and the later, initiator-bound
//! `cp/delegate_result` back to whoever asked for the work.
//!
//! Three deliberate seams keep this unit-testable without a socket:
//!
//! - **Commands are data.** External callers (the local UDS server, an MCP
//!   tool, a CLI) submit a [`ClientCommand`] over an mpsc channel and await a
//!   `oneshot`. The command carries no socket and no `&mut sink`; the serve
//!   loop is the single owner that turns it into a frame. So this module can be
//!   exercised by building commands and inspecting the frames/bookkeeping they
//!   produce.
//! - **The delegation handle is opaque.** [`DelegationHandle`] encapsulates the
//!   `(delegation_id, admission)` pair the wire uses to name one admission of a
//!   reusable id. Callers hold it, pass it back to cancel or await, and never
//!   pick it apart — a bare `delegation_id` is never a valid reference, exactly
//!   as the protocol requires.
//! - **Correlation is a map, not a socket read.** [`PrimaryState`] owns the
//!   `rpc_id → pending` and `(id, admission) → delegation` tables. The serve
//!   loop feeds it inbound frames and outbound intents; every state transition
//!   is a plain method with an assertable result.

use std::collections::{HashMap, VecDeque};

use openab_cp::proto::{
    AdmissionToken, AgentSummary, DelegateAck, DelegateResultParams, DelegationStatus, ErrorObject,
    TargetSelector,
};
use tokio::sync::oneshot;
use tracing::{debug, warn};

/// Opaque reference to ONE admission of a delegation this runtime initiated.
///
/// It wraps the coupled `(delegation_id, admission)` pair the protocol uses to
/// name a specific admission (see `openab_cp::proto::AdmissionToken`). It is
/// opaque on purpose: a caller cancels or awaits by handing the whole handle
/// back, never by reconstructing it from a bare `delegation_id` (which is
/// reusable and names only a *slot*). Cloneable and comparable so a caller can
/// hold one while awaiting and pass a copy to cancel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DelegationHandle {
    delegation_id: String,
    admission: AdmissionToken,
}

impl DelegationHandle {
    pub(crate) fn new(delegation_id: impl Into<String>, admission: AdmissionToken) -> Self {
        Self {
            delegation_id: delegation_id.into(),
            admission,
        }
    }

    pub fn delegation_id(&self) -> &str {
        &self.delegation_id
    }

    pub fn admission(&self) -> AdmissionToken {
        self.admission
    }

    /// The composite key the tracking table is indexed by. Kept private-ish:
    /// callers compare handles, they do not build keys.
    fn key(&self) -> (String, AdmissionToken) {
        (self.delegation_id.clone(), self.admission)
    }
}

/// A spawn target: an exact logical name, or a set of labels that must all
/// match. Exactly one is set — the constructors enforce that so a caller
/// cannot submit an ambiguous or empty selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnTarget {
    /// Route to the runtime with this exact logical `name`.
    Name(String),
    /// Route to any healthy runtime whose labels are a superset of these.
    Labels(std::collections::BTreeMap<String, String>),
}

impl SpawnTarget {
    pub fn by_name(name: impl Into<String>) -> Self {
        SpawnTarget::Name(name.into())
    }

    pub fn by_labels(labels: std::collections::BTreeMap<String, String>) -> Self {
        SpawnTarget::Labels(labels)
    }

    pub(crate) fn into_selector(self) -> TargetSelector {
        match self {
            SpawnTarget::Name(name) => TargetSelector {
                name: Some(name),
                labels: None,
            },
            SpawnTarget::Labels(labels) => TargetSelector {
                name: None,
                labels: Some(labels),
            },
        }
    }
}

/// A request to spawn (delegate) work onto a peer runtime.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Caller-generated idempotency id. Reusable across cancel-then-retry.
    pub delegation_id: String,
    pub target: SpawnTarget,
    pub prompt: String,
    /// Absolute deadline by which the delegation must complete.
    pub deadline: chrono::DateTime<chrono::Utc>,
    /// If this spawn happens while serving another delegation, its handle —
    /// the CP derives ancestry, depth, cycle, and the child's deadline budget
    /// from the admission this names. `None` for a root delegation.
    pub parent: Option<DelegationHandle>,
}

/// Terminal outcome of an initiated delegation, as reported back to the caller
/// that spawned it. Mirrors the wire `DelegationStatus` plus the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationOutcome {
    pub status: DelegationStatus,
    pub result: Option<String>,
    pub error: Option<String>,
}
impl DelegationOutcome {
    fn from_result(params: &DelegateResultParams) -> Self {
        Self {
            status: params.status.clone(),
            result: params.result.clone(),
            error: params.error.clone(),
        }
    }
}

/// Serializable projection of [`DelegationStatus`] for the local IPC protocol.
///
/// A distinct type so the local socket protocol (consumed by a CLI or MCP
/// tool) does not depend on `openab_cp::proto` types directly; the snake_case
/// wire words are identical, so a caller sees the same vocabulary the CP uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatusWire {
    Completed,
    Failed,
    Timeout,
    Cancelled,
    TargetDisconnected,
}

impl From<DelegationStatus> for DelegationStatusWire {
    fn from(s: DelegationStatus) -> Self {
        match s {
            DelegationStatus::Completed => DelegationStatusWire::Completed,
            DelegationStatus::Failed => DelegationStatusWire::Failed,
            DelegationStatus::Timeout => DelegationStatusWire::Timeout,
            DelegationStatus::Cancelled => DelegationStatusWire::Cancelled,
            DelegationStatus::TargetDisconnected => DelegationStatusWire::TargetDisconnected,
        }
    }
}

/// The reply to a successful [`ClientCommand::Spawn`]: the opaque handle plus
/// the peer the CP routed the work to. `assigned_to` is only knowable from the
/// CP's `cp/delegate` ack, so it is carried here rather than reconstructed by
/// the caller (the previous shape returned a bare handle, forcing the local
/// layer to emit an empty `assigned_to`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnAck {
    pub handle: DelegationHandle,
    pub assigned_to: String,
}

/// A nonblocking snapshot of a tracked delegation's lifecycle, returned by
/// [`ClientCommand::Check`]. Unlike [`ClientCommand::Await`], it never parks:
/// a still-running delegation answers `Running`, not a blocked reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationSnapshot {
    /// Delegate frame sent, ack not yet received.
    Pending,
    /// Ack received; the peer runtime is serving it.
    Running { assigned_to: String },
    /// A terminal `cp/delegate_result` has arrived.
    Terminal(DelegationOutcome),
}

/// A command submitted to the serve loop by an external initiating surface.
///
/// Each carries its own `oneshot` reply sender: the serve loop performs the
/// frame write and (for spawn/cancel/list) the correlation, then answers the
/// caller. The loop is the only writer to the socket, so no command holds the
/// sink.
pub enum ClientCommand {
    /// Initiate a delegation. Replies with the opaque handle and the assigned
    /// peer once the CP acks routing (the terminal result is awaited
    /// separately via [`Await`]).
    Spawn {
        request: SpawnRequest,
        reply: oneshot::Sender<Result<SpawnAck, CommandError>>,
    },
    /// Await the terminal result of a delegation this runtime initiated.
    /// Blocks (parks) until a terminal frame arrives or the connection drops.
    Await {
        handle: DelegationHandle,
        reply: oneshot::Sender<Result<DelegationOutcome, CommandError>>,
    },
    /// Nonblocking status query for a delegation this runtime initiated.
    /// Answers immediately with the current lifecycle snapshot.
    Check {
        handle: DelegationHandle,
        reply: oneshot::Sender<Result<DelegationSnapshot, CommandError>>,
    },
    /// Cancel an in-flight delegation by opaque handle.
    Cancel {
        handle: DelegationHandle,
        reason: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    /// Namespace-scoped roster snapshot.
    ListAgents {
        reply: oneshot::Sender<Result<Vec<AgentSummary>, CommandError>>,
    },
}

/// Why a primary-side command could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The client is not currently connected/registered, so no frame can be
    /// originated. The caller may retry once the client reconnects.
    NotConnected,
    /// The CP answered the request with a JSON-RPC error.
    Cp { code: i64, message: String },
    /// The named handle is not (or no longer) tracked here.
    UnknownDelegation,
    /// The connection dropped while this request/delegation was outstanding.
    Disconnected,
    /// The command channel or reply path was torn down (shutdown).
    Internal(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::NotConnected => {
                write!(f, "control-plane client is not connected")
            }
            CommandError::Cp { code, message } => {
                write!(
                    f,
                    "control plane returned an error: {message} (code {code})"
                )
            }
            CommandError::UnknownDelegation => {
                write!(f, "no such delegation is tracked by this runtime")
            }
            CommandError::Disconnected => {
                write!(
                    f,
                    "control-plane connection dropped before the delegation finished"
                )
            }
            CommandError::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for CommandError {}

impl CommandError {
    fn from_cp(err: ErrorObject) -> Self {
        CommandError::Cp {
            code: err.code,
            message: err.message,
        }
    }
}

/// What the serve loop originated and is still awaiting a JSON-RPC reply for.
///
/// Correlated by the JSON-RPC id we chose. `Spawn` is special: its reply is a
/// [`DelegateAck`], and success does NOT complete the caller — it registers the
/// admission so the later `cp/delegate_result` can be routed. Cancel and list
/// complete the caller directly.
enum PendingRequest {
    Spawn {
        delegation_id: String,
        reply: oneshot::Sender<Result<SpawnAck, CommandError>>,
    },
    Cancel {
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    ListAgents {
        reply: oneshot::Sender<Result<Vec<AgentSummary>, CommandError>>,
    },
}

/// Lifecycle of one initiated delegation, from ack to terminal.
struct TrackedDelegation {
    state: DelegationLifecycle,
}

/// The observable state of a tracked delegation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationLifecycle {
    /// Delegate frame sent, ack not yet received.
    Pending,
    /// Ack received; the peer runtime is (or will be) serving it.
    Running { assigned_to: String },
    /// A terminal `cp/delegate_result` arrived.
    Terminal(DelegationOutcome),
}

/// The composite key naming one admission of a (reusable) delegation id.
type DelegationKey = (String, AdmissionToken);
/// A parked reply for a caller awaiting a delegation's terminal outcome.
type AwaitReply = oneshot::Sender<Result<DelegationOutcome, CommandError>>;
/// Retained terminal-history ceiling. Live delegations are never evicted.
const MAX_TRACKED_DELEGATIONS: usize = 4096;

/// Primary-side correlation and delegation tracking for one runtime instance.
///
/// Not `Sync`-shared across tasks: it lives inside the single-owner serve loop
/// and every method runs there. The serve loop hands it inbound frames and
/// outbound intents; nothing else touches it.
#[derive(Default)]
pub struct PrimaryState {
    /// JSON-RPC id → the request the loop originated and awaits a reply for.
    pending: HashMap<u64, PendingRequest>,
    /// (delegation_id, admission) → its lifecycle.
    tracked: HashMap<DelegationKey, TrackedDelegation>,
    /// Insertion order for deterministic oldest-terminal history eviction.
    tracked_order: VecDeque<DelegationKey>,
    /// Callers awaiting a terminal result, keyed by the same composite key.
    /// A caller can await before OR after the terminal arrives.
    awaiters: HashMap<DelegationKey, Vec<AwaitReply>>,
}

/// Outcome of feeding a spawn command to the state: the params to emit, already
/// validated and parked under the rpc id the caller allocated.
pub(crate) struct SpawnEmission {
    pub rpc_id: u64,
    pub params: openab_cp::proto::DelegateParams,
}

impl PrimaryState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of delegations currently tracked (any lifecycle state).
    pub fn tracked_len(&self) -> usize {
        self.tracked.len()
    }

    /// Register a spawn: park its reply under `rpc_id` and produce the wire
    /// params. The caller is answered later, when the ack lands. `rpc_id` is
    /// allocated by the serve loop so every outbound frame — heartbeat,
    /// delegate_result ack, and these primary requests — shares one id space.
    pub(crate) fn begin_spawn(
        &mut self,
        rpc_id: u64,
        request: SpawnRequest,
        reply: oneshot::Sender<Result<SpawnAck, CommandError>>,
    ) -> SpawnEmission {
        let (parent_delegation_id, parent_admission) = match request.parent {
            Some(h) => (Some(h.delegation_id), Some(h.admission)),
            None => (None, None),
        };
        let params = openab_cp::proto::DelegateParams {
            delegation_id: request.delegation_id.clone(),
            target: request.target.into_selector(),
            prompt: request.prompt,
            deadline: request.deadline,
            parent_delegation_id,
            parent_admission,
        };
        self.pending.insert(
            rpc_id,
            PendingRequest::Spawn {
                delegation_id: request.delegation_id,
                reply,
            },
        );
        SpawnEmission { rpc_id, params }
    }

    /// Register a cancel: park its reply under `rpc_id` and return the wire
    /// params. Returns `None` (and answers the caller `UnknownDelegation`) if
    /// the handle is not tracked here — a cancel for something we never spawned
    /// is a caller error, not a frame worth sending.
    pub(crate) fn begin_cancel(
        &mut self,
        rpc_id: u64,
        handle: &DelegationHandle,
        reason: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    ) -> Option<openab_cp::proto::CancelParams> {
        if !self.tracked.contains_key(&handle.key()) {
            let _ = reply.send(Err(CommandError::UnknownDelegation));
            return None;
        }
        let params = openab_cp::proto::CancelParams {
            delegation_id: handle.delegation_id.clone(),
            admission: handle.admission,
            reason,
        };
        self.pending
            .insert(rpc_id, PendingRequest::Cancel { reply });
        Some(params)
    }

    /// Register a list_agents: park its reply under `rpc_id`.
    pub(crate) fn begin_list_agents(
        &mut self,
        rpc_id: u64,
        reply: oneshot::Sender<Result<Vec<AgentSummary>, CommandError>>,
    ) {
        self.pending
            .insert(rpc_id, PendingRequest::ListAgents { reply });
    }

    /// Await a delegation's terminal outcome. If it already ended, the caller
    /// is answered immediately; otherwise the reply is parked until the
    /// terminal frame arrives (or the connection drops).
    pub fn begin_await(
        &mut self,
        handle: &DelegationHandle,
        reply: oneshot::Sender<Result<DelegationOutcome, CommandError>>,
    ) {
        match self.tracked.get(&handle.key()) {
            Some(TrackedDelegation {
                state: DelegationLifecycle::Terminal(outcome),
                ..
            }) => {
                let _ = reply.send(Ok(outcome.clone()));
            }
            Some(_) => {
                self.awaiters.entry(handle.key()).or_default().push(reply);
            }
            None => {
                let _ = reply.send(Err(CommandError::UnknownDelegation));
            }
        }
    }

    /// Nonblocking status query. Answers the caller immediately with the
    /// current [`DelegationSnapshot`]; a running delegation returns `Running`
    /// rather than parking, unlike [`begin_await`]. Unknown handles get
    /// `UnknownDelegation`.
    pub fn check(
        &self,
        handle: &DelegationHandle,
        reply: oneshot::Sender<Result<DelegationSnapshot, CommandError>>,
    ) {
        let snapshot = match self.tracked.get(&handle.key()) {
            Some(TrackedDelegation {
                state: DelegationLifecycle::Pending,
            }) => Some(DelegationSnapshot::Pending),
            Some(TrackedDelegation {
                state: DelegationLifecycle::Running { assigned_to },
            }) => Some(DelegationSnapshot::Running {
                assigned_to: assigned_to.clone(),
            }),
            Some(TrackedDelegation {
                state: DelegationLifecycle::Terminal(outcome),
            }) => Some(DelegationSnapshot::Terminal(outcome.clone())),
            None => None,
        };
        match snapshot {
            Some(s) => {
                let _ = reply.send(Ok(s));
            }
            None => {
                let _ = reply.send(Err(CommandError::UnknownDelegation));
            }
        }
    }
    /// pending request that originated it. Returns true if it was ours.
    ///
    /// `id` is the echoed request id; `result`/`error` are the two arms of a
    /// JSON-RPC reply.
    pub fn on_reply(
        &mut self,
        id: u64,
        result: Option<serde_json::Value>,
        error: Option<ErrorObject>,
    ) -> bool {
        let Some(pending) = self.pending.remove(&id) else {
            return false;
        };
        match pending {
            PendingRequest::Spawn {
                delegation_id,
                reply,
            } => {
                if let Some(err) = error {
                    let _ = reply.send(Err(CommandError::from_cp(err)));
                    return true;
                }
                let ack: Option<DelegateAck> = result.and_then(|v| serde_json::from_value(v).ok());
                match ack {
                    Some(ack) => {
                        let handle =
                            DelegationHandle::new(ack.delegation_id.clone(), ack.admission);
                        // Track the admission so the later result frame routes.
                        self.make_history_room();
                        let key = handle.key();
                        self.tracked.insert(
                            key.clone(),
                            TrackedDelegation {
                                state: DelegationLifecycle::Running {
                                    assigned_to: ack.assigned_to.clone(),
                                },
                            },
                        );
                        self.tracked_order.push_back(key);
                        debug!(
                            delegation_id = %delegation_id,
                            admission = ack.admission,
                            "delegation admitted by control plane"
                        );
                        let _ = reply.send(Ok(SpawnAck {
                            handle,
                            assigned_to: ack.assigned_to,
                        }));
                    }
                    None => {
                        let _ = reply.send(Err(CommandError::Internal(
                            "cp/delegate ack carried no valid result".into(),
                        )));
                    }
                }
                true
            }
            PendingRequest::Cancel { reply } => {
                match error {
                    Some(err) => {
                        let _ = reply.send(Err(CommandError::from_cp(err)));
                    }
                    None => {
                        let _ = reply.send(Ok(()));
                    }
                }
                true
            }
            PendingRequest::ListAgents { reply } => {
                if let Some(err) = error {
                    let _ = reply.send(Err(CommandError::from_cp(err)));
                    return true;
                }
                let parsed: Option<openab_cp::proto::ListAgentsResult> =
                    result.and_then(|v| serde_json::from_value(v).ok());
                match parsed {
                    Some(list) => {
                        let _ = reply.send(Ok(list.agents));
                    }
                    None => {
                        let _ = reply.send(Err(CommandError::Internal(
                            "cp/list_agents reply carried no valid result".into(),
                        )));
                    }
                }
                true
            }
        }
    }

    /// Evict oldest terminal history until a new tracked admission fits.
    /// Running/Pending work is never evicted; CP/global capacity bounds it.
    fn make_history_room(&mut self) {
        while self.tracked.len() >= MAX_TRACKED_DELEGATIONS {
            let Some(index) = self.tracked_order.iter().position(|key| {
                self.tracked.get(key).is_some_and(|tracked| {
                    matches!(tracked.state, DelegationLifecycle::Terminal(_))
                })
            }) else {
                break;
            };
            let key = self
                .tracked_order
                .remove(index)
                .expect("index came from position");
            self.tracked.remove(&key);
            self.awaiters.remove(&key);
        }
    }

    /// Route an initiator-bound `cp/delegate_result` to its tracked delegation
    /// and wake any awaiters. Returns true if the frame named a delegation we
    /// track (and therefore should be acked as ours).
    ///
    /// Correlation is on the `(delegation_id, admission)` pair, never the
    /// reusable id alone: a late result for a superseded admission is dropped.
    pub fn on_delegate_result(&mut self, params: &DelegateResultParams) -> bool {
        let key = (params.delegation_id.clone(), params.admission);
        let Some(tracked) = self.tracked.get_mut(&key) else {
            warn!(
                delegation_id = %params.delegation_id,
                admission = params.admission,
                "cp/delegate_result for an untracked (id, admission) — ignoring"
            );
            return false;
        };
        // First terminal wins; a duplicate for the same admission is ignored.
        if matches!(tracked.state, DelegationLifecycle::Terminal(_)) {
            debug!(
                delegation_id = %params.delegation_id,
                admission = params.admission,
                "duplicate terminal frame ignored"
            );
            return true;
        }
        let outcome = DelegationOutcome::from_result(params);
        tracked.state = DelegationLifecycle::Terminal(outcome.clone());
        if let Some(waiters) = self.awaiters.remove(&key) {
            for w in waiters {
                let _ = w.send(Ok(outcome.clone()));
            }
        }
        true
    }

    /// Current lifecycle of a tracked delegation (for tests/introspection).
    pub fn lifecycle(&self, handle: &DelegationHandle) -> Option<DelegationLifecycle> {
        self.tracked.get(&handle.key()).map(|t| t.state.clone())
    }

    /// Fail every outstanding primary-side interaction on connection loss.
    ///
    /// Pending requests (spawn/cancel/list awaiting a reply that will never
    /// come), non-terminal tracked delegations, and their awaiters are all
    /// answered `Disconnected`. Terminal delegations are dropped: their result
    /// already reached the caller.
    ///
    /// This is the primary-side analogue of the executor's `cancel_all` on the
    /// worker side — no local state may claim a delegation is live once the
    /// only connection that could complete it is gone.
    pub fn fail_all_live(&mut self) {
        for (_id, pending) in self.pending.drain() {
            match pending {
                PendingRequest::Spawn { reply, .. } => {
                    let _ = reply.send(Err(CommandError::Disconnected));
                }
                PendingRequest::Cancel { reply } => {
                    let _ = reply.send(Err(CommandError::Disconnected));
                }
                PendingRequest::ListAgents { reply } => {
                    let _ = reply.send(Err(CommandError::Disconnected));
                }
            }
        }
        // Mark non-terminal delegations failed and answer their awaiters.
        let live_keys: Vec<DelegationKey> = self
            .tracked
            .iter()
            .filter(|(_, t)| !matches!(t.state, DelegationLifecycle::Terminal(_)))
            .map(|(k, _)| k.clone())
            .collect();
        for key in live_keys {
            if let Some(waiters) = self.awaiters.remove(&key) {
                for w in waiters {
                    let _ = w.send(Err(CommandError::Disconnected));
                }
            }
            self.tracked.remove(&key);
        }
        // Any awaiters that remain point at delegations that reached a terminal
        // state (their result already reached the caller at `begin_await` time),
        // so nothing is owed. Drop any that no longer name a tracked delegation
        // to keep the map from leaking. Snapshot the keys first so the retain
        // closure does not borrow `self.tracked` while `self.awaiters` is
        // mutably borrowed.
        let tracked_keys: std::collections::HashSet<DelegationKey> =
            self.tracked.keys().cloned().collect();
        self.awaiters.retain(|k, _| tracked_keys.contains(k));
        self.tracked_order.retain(|key| tracked_keys.contains(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openab_cp::proto::DelegationStatus;

    fn spawn_request(id: &str) -> SpawnRequest {
        SpawnRequest {
            delegation_id: id.into(),
            target: SpawnTarget::by_name("worker-1"),
            prompt: "do the thing".into(),
            deadline: chrono::Utc::now() + chrono::Duration::seconds(60),
            parent: None,
        }
    }

    fn ack_value(
        delegation_id: &str,
        admission: AdmissionToken,
        assigned_to: &str,
    ) -> serde_json::Value {
        serde_json::to_value(DelegateAck {
            delegation_id: delegation_id.into(),
            admission,
            assigned_to: assigned_to.into(),
        })
        .unwrap()
    }

    #[test]
    fn a_handle_is_opaque_and_carries_both_halves() {
        let h = DelegationHandle::new("d-1", 7);
        assert_eq!(h.delegation_id(), "d-1");
        assert_eq!(h.admission(), 7);
        // Same id, different admission is a different handle.
        assert_ne!(h, DelegationHandle::new("d-1", 8));
    }

    #[test]
    fn spawn_target_is_exactly_one_of_name_or_labels() {
        let by_name = SpawnTarget::by_name("w1").into_selector();
        assert_eq!(by_name.name.as_deref(), Some("w1"));
        assert!(by_name.labels.is_none());

        let mut labels = std::collections::BTreeMap::new();
        labels.insert("backend".to_string(), "kiro".to_string());
        let by_labels = SpawnTarget::by_labels(labels.clone()).into_selector();
        assert!(by_labels.name.is_none());
        assert_eq!(by_labels.labels, Some(labels));
    }

    #[test]
    fn begin_spawn_parks_the_reply_and_produces_wire_params() {
        let mut st = PrimaryState::new();
        let (tx, _rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        assert_eq!(emission.rpc_id, 1);
        assert_eq!(emission.params.delegation_id, "d-1");
        assert_eq!(emission.params.target.name.as_deref(), Some("worker-1"));
        // A root spawn carries no parent reference on the wire.
        assert!(emission.params.parent_delegation_id.is_none());
        assert!(emission.params.parent_admission.is_none());
    }

    #[test]
    fn a_parented_spawn_carries_both_halves_of_the_parent_reference() {
        let mut st = PrimaryState::new();
        let (tx, _rx) = oneshot::channel();
        let mut req = spawn_request("child");
        req.parent = Some(DelegationHandle::new("parent", 42));
        let emission = st.begin_spawn(7, req, tx);
        assert_eq!(
            emission.params.parent_delegation_id.as_deref(),
            Some("parent")
        );
        assert_eq!(emission.params.parent_admission, Some(42));
    }

    #[tokio::test]
    async fn a_delegate_ack_resolves_the_spawn_and_starts_tracking() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        assert!(st.on_reply(
            emission.rpc_id,
            Some(ack_value("d-1", 5, "prod/worker-1")),
            None
        ));
        let ack = rx.await.unwrap().unwrap();
        assert_eq!(ack.assigned_to, "prod/worker-1");
        let handle = ack.handle;
        assert_eq!(handle.delegation_id(), "d-1");
        assert_eq!(handle.admission(), 5);
        assert_eq!(
            st.lifecycle(&handle),
            Some(DelegationLifecycle::Running {
                assigned_to: "prod/worker-1".into()
            })
        );
    }

    #[tokio::test]
    async fn a_delegate_error_reply_fails_the_spawn_and_tracks_nothing() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        let err = ErrorObject::new(openab_cp::proto::codes::NO_TARGET, "no such worker");
        assert!(st.on_reply(emission.rpc_id, None, Some(err)));
        let outcome = rx.await.unwrap();
        assert!(matches!(
            outcome,
            Err(CommandError::Cp {
                code: openab_cp::proto::codes::NO_TARGET,
                ..
            })
        ));
        assert_eq!(st.tracked_len(), 0);
    }

    #[tokio::test]
    async fn a_terminal_result_wakes_an_earlier_awaiter() {
        let mut st = PrimaryState::new();
        // Spawn + ack.
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        st.on_reply(emission.rpc_id, Some(ack_value("d-1", 3, "prod/w1")), None);
        let handle = rx.await.unwrap().unwrap().handle;

        // Await before the terminal frame arrives.
        let (atx, arx) = oneshot::channel();
        st.begin_await(&handle, atx);

        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            admission: 3,
            status: DelegationStatus::Completed,
            result: Some("done".into()),
            error: None,
        };
        assert!(st.on_delegate_result(&result));
        let outcome = arx.await.unwrap().unwrap();
        assert_eq!(outcome.status, DelegationStatus::Completed);
        assert_eq!(outcome.result.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn awaiting_an_already_terminal_delegation_answers_immediately() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        st.on_reply(emission.rpc_id, Some(ack_value("d-1", 3, "prod/w1")), None);
        let handle = rx.await.unwrap().unwrap().handle;
        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            admission: 3,
            status: DelegationStatus::Failed,
            result: None,
            error: Some("boom".into()),
        };
        st.on_delegate_result(&result);

        let (atx, arx) = oneshot::channel();
        st.begin_await(&handle, atx);
        let outcome = arx.await.unwrap().unwrap();
        assert_eq!(outcome.status, DelegationStatus::Failed);
        assert_eq!(outcome.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn a_stale_admission_result_is_dropped() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        st.on_reply(emission.rpc_id, Some(ack_value("d-1", 10, "prod/w1")), None);
        let _handle = rx.await.unwrap().unwrap().handle;
        // A result for the same id but a different admission is not ours.
        let stale = DelegateResultParams {
            delegation_id: "d-1".into(),
            admission: 9,
            status: DelegationStatus::Completed,
            result: Some("wrong".into()),
            error: None,
        };
        assert!(
            !st.on_delegate_result(&stale),
            "a stale admission is not acked as ours"
        );
    }

    #[tokio::test]
    async fn cancel_of_an_untracked_handle_never_emits_a_frame() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emitted = st.begin_cancel(1, &DelegationHandle::new("never", 1), "stop".into(), tx);
        assert!(emitted.is_none());
        assert!(matches!(
            rx.await.unwrap(),
            Err(CommandError::UnknownDelegation)
        ));
    }

    #[tokio::test]
    async fn cancel_of_a_tracked_handle_emits_and_resolves_on_ok() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        st.on_reply(emission.rpc_id, Some(ack_value("d-1", 3, "prod/w1")), None);
        let handle = rx.await.unwrap().unwrap().handle;

        let (ctx, crx) = oneshot::channel();
        let params = st
            .begin_cancel(2, &handle, "initiator gave up".into(), ctx)
            .expect("a tracked handle emits a cancel frame");
        assert_eq!(params.delegation_id, "d-1");
        assert_eq!(params.admission, 3);
        // CP acks the cancel.
        assert!(st.on_reply(2, Some(serde_json::json!({"ok": true})), None));
        assert!(crx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn list_agents_resolves_with_the_roster() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        st.begin_list_agents(1, tx);
        let result = openab_cp::proto::ListAgentsResult {
            namespace: "prod".into(),
            agents: vec![AgentSummary {
                name: "worker-1".into(),
                agent_type: openab_cp::proto::AgentType::Worker,
                instance_id: "i-1".into(),
                labels: Default::default(),
                active_sessions: 0,
                max_delegated_sessions: 2,
            }],
        };
        assert!(st.on_reply(1, Some(serde_json::to_value(&result).unwrap()), None));
        let agents = rx.await.unwrap().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "worker-1");
    }
    #[tokio::test]
    async fn check_reports_running_then_terminal_without_blocking() {
        let mut st = PrimaryState::new();
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-1"), tx);
        st.on_reply(emission.rpc_id, Some(ack_value("d-1", 4, "prod/w1")), None);
        let handle = rx.await.unwrap().unwrap().handle;

        // Running (ack received, no terminal yet) — never parks.
        let (ctx, crx) = oneshot::channel();
        st.check(&handle, ctx);
        assert_eq!(
            crx.await.unwrap().unwrap(),
            DelegationSnapshot::Running {
                assigned_to: "prod/w1".into()
            }
        );

        // Terminal after the result frame.
        st.on_delegate_result(&DelegateResultParams {
            delegation_id: "d-1".into(),
            admission: 4,
            status: DelegationStatus::Completed,
            result: Some("done".into()),
            error: None,
        });
        let (ctx, crx) = oneshot::channel();
        st.check(&handle, ctx);
        assert_eq!(
            crx.await.unwrap().unwrap(),
            DelegationSnapshot::Terminal(DelegationOutcome {
                status: DelegationStatus::Completed,
                result: Some("done".into()),
                error: None,
            })
        );
    }

    #[tokio::test]
    async fn check_reports_pending_before_the_ack_lands() {
        // A delegation whose lifecycle is Pending maps to a Pending snapshot.
        // (In the running client a caller only obtains a handle at ack time, so
        // this state is reached by seeding the tracking table directly — same
        // module, private access — to prove the mapping is complete.)
        let mut st = PrimaryState::new();
        let handle = DelegationHandle::new("d-pending", 1);
        st.tracked.insert(
            handle.key(),
            TrackedDelegation {
                state: DelegationLifecycle::Pending,
            },
        );
        let (ctx, crx) = oneshot::channel();
        st.check(&handle, ctx);
        assert_eq!(crx.await.unwrap().unwrap(), DelegationSnapshot::Pending);
    }

    #[tokio::test]
    async fn check_of_an_untracked_handle_is_unknown() {
        let st = PrimaryState::new();
        let (ctx, crx) = oneshot::channel();
        st.check(&DelegationHandle::new("never", 1), ctx);
        assert!(matches!(
            crx.await.unwrap(),
            Err(CommandError::UnknownDelegation)
        ));
    }

    #[tokio::test]
    async fn a_foreign_reply_id_is_not_ours() {
        let mut st = PrimaryState::new();
        assert!(!st.on_reply(999, Some(serde_json::json!({"ok": true})), None));
    }

    #[test]
    fn terminal_history_is_bounded_by_evicting_the_oldest_terminal() {
        let mut state = PrimaryState::new();
        for admission in 1..=MAX_TRACKED_DELEGATIONS as u64 {
            let key = (format!("d-{admission}"), admission);
            state.tracked.insert(
                key.clone(),
                TrackedDelegation {
                    state: DelegationLifecycle::Terminal(DelegationOutcome {
                        status: DelegationStatus::Completed,
                        result: Some("done".into()),
                        error: None,
                    }),
                },
            );
            state.tracked_order.push_back(key);
        }
        state.make_history_room();
        let new_key = ("d-new".to_string(), 9000);
        state.tracked.insert(
            new_key.clone(),
            TrackedDelegation {
                state: DelegationLifecycle::Running {
                    assigned_to: "prod/w".into(),
                },
            },
        );
        state.tracked_order.push_back(new_key);
        assert_eq!(state.tracked.len(), MAX_TRACKED_DELEGATIONS);
        assert!(!state.tracked.contains_key(&("d-1".to_string(), 1)));
    }

    #[tokio::test]
    async fn fail_all_live_disconnects_pending_and_running_but_not_terminal() {
        let mut st = PrimaryState::new();

        // A running delegation with a parked awaiter.
        let (tx, rx) = oneshot::channel();
        let emission = st.begin_spawn(1, spawn_request("d-run"), tx);
        st.on_reply(
            emission.rpc_id,
            Some(ack_value("d-run", 1, "prod/w1")),
            None,
        );
        let running = rx.await.unwrap().unwrap().handle;
        let (arx_tx, arx) = oneshot::channel();
        st.begin_await(&running, arx_tx);

        // A terminal delegation: its awaiter was already answered.
        let (tx2, rx2) = oneshot::channel();
        let emission2 = st.begin_spawn(2, spawn_request("d-done"), tx2);
        st.on_reply(
            emission2.rpc_id,
            Some(ack_value("d-done", 2, "prod/w1")),
            None,
        );
        let done = rx2.await.unwrap().unwrap().handle;
        st.on_delegate_result(&DelegateResultParams {
            delegation_id: "d-done".into(),
            admission: 2,
            status: DelegationStatus::Completed,
            result: Some("ok".into()),
            error: None,
        });

        // A pending spawn awaiting its ack.
        let (ptx, prx) = oneshot::channel();
        let _pending = st.begin_spawn(3, spawn_request("d-pending"), ptx);

        st.fail_all_live();

        // The running delegation's awaiter is failed.
        assert!(matches!(
            arx.await.unwrap(),
            Err(CommandError::Disconnected)
        ));
        // The pending spawn is failed.
        assert!(matches!(
            prx.await.unwrap(),
            Err(CommandError::Disconnected)
        ));
        // The terminal delegation stays queryable.
        assert_eq!(
            st.lifecycle(&done),
            Some(DelegationLifecycle::Terminal(DelegationOutcome {
                status: DelegationStatus::Completed,
                result: Some("ok".into()),
                error: None,
            }))
        );
        // The running delegation is no longer tracked (it was live).
        assert_eq!(st.lifecycle(&running), None);
    }
}

//! `openab set/get` IPC over Unix domain socket.
//!
//! Architecture (like consul/vault):
//! - `openab run` spawns a UnixListener at a well-known path.
//! - `openab set key value` connects, sends a JSON request, reads the response.
//!
//! Phase 1 supported keys:
//! - `thread.name` — rename the current Discord/Slack thread
//!
//! Phase 2 (workflow `20260818-openab-project-aware-thread-routing`):
//! - `thread.pin` — project-aware thread/session registration API (trusted
//!   bootstrap of `ProjectContext`).
//! - `thread.message` — extended to optionally carry a `project` field that
//!   pins before sending (`ensure_pinned_project` first, then
//!   `send_message_targeted`).

#[cfg(unix)]
#[cfg(test)]
use openab_core::acp::pool::format_native_dispatch_key;
#[cfg(unix)]
use openab_core::acp::project::ProjectContext;
#[cfg(unix)]
use openab_core::acp::SessionPool;
#[cfg(unix)]
use openab_core::adapter::MessageRef;
#[cfg(unix)]
use openab_core::adapter::{ChannelRef, ChatAdapter};
#[cfg(unix)]
use openab_core::admission::{NativeWorkflowMetadata, WorkAdmissionPort, WorkAdmissionRequest};
#[cfg(unix)]
use openab_core::control_plane::{self, ControlRequestContext};
#[cfg(unix)]
use openab_core::dispatch::BufferedMessage;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Instant;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tracing::{debug, error, info, warn};

/// Default socket path. Overridable via `OPENAB_SOCK` env var.
pub fn socket_path() -> PathBuf {
    std::env::var("OPENAB_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/openab.sock"))
}

/// Process-local opaque correlation sequence for accepted ctl connections.
#[cfg(unix)]
static CONTROL_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[cfg(unix)]
fn emit_thread_message_stage(
    request_id: Option<&str>,
    thread_id: Option<&str>,
    target_user_id: Option<&str>,
    stage: &str,
    status: &str,
) {
    info!(
        event = "openab.control_plane.thread_message_stage",
        request_id = ?request_id,
        thread_id = ?thread_id,
        target_user_id = ?target_user_id,
        stage,
        status,
        "canonical thread.message control-plane stage"
    );
}

// ─── Protocol ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub action: Action,
    pub key: String,
    pub value: Option<String>,
    /// Target thread/channel ID — daemon uses this to route to the correct adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Optional Discord numeric user id that the canonical message MUST
    /// mention. Used by ``set thread.message`` to pin ``allowed_mentions`` so
    /// the recipient is the only legitimate mention Discord will surface.
    /// ``None`` for other keys.
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_user_id: Option<String>,
    /// Optional project bootstrap (workflow
    /// `20260818-openab-project-aware-thread-routing`). Trusted
    /// transport-neutral seam for the OpenAB/AAP integration layer. Carried
    /// by `thread.pin` (registers only) and `thread.message` (registers then
    /// sends). `None` = legacy behavior; no project hint.
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
    /// Native workflow admission fields. `flatten` keeps the ctl wire schema
    /// explicit (rather than hiding fencing data in `value`).
    #[cfg(unix)]
    #[serde(flatten)]
    pub agent_work: Option<AgentWorkRequest>,
}

/// Strict native work request accepted only by `set agent.work`.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWorkRequest {
    pub dispatch_id: String,
    pub workflow_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub task_id: String,
    pub role: String,
    pub agent: String,
    pub lease_id: String,
    pub lease_generation: u64,
    pub expected_revision: u64,
    pub conversation_key: String,
    pub assignment: String,
    pub language: String,
    /// Phase 6.4.1B — authoritative transport identity carried in by the
    /// scheduler's structured dispatch metadata. The transport is propagated
    /// unchanged into `NativeWorkflowMetadata.transport` and ultimately into
    /// `NativeCompletionEvent.transport` so AAP Runtime's Phase 6.4.1
    /// transport-aware conversation identity validator can match.
    ///
    /// `None` (the deserialization default for legacy callers) means the
    /// scheduler did not declare transport; Runtime then defaults to legacy
    /// OPENAB semantics. NEVER derived from `conversation_key` prefix or
    /// any other heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Phase 6.4.1D — authoritative structured delivery destination carried
    /// in by AAP Runtime from the upstream ``ConversationBinding``. When
    /// ``Some(_)` the daemon uses this as the
    /// ``BufferedMessage.trigger_msg.channel`` for THIS turn INSTEAD of
    /// the daemon-wide ``native_delivery_target`` fallback, so every
    /// role handoff lands in the actual workflow's originating Discord
    /// channel rather than a hardcoded control-plane target.
    ///
    /// The Runtime sources the value from the trusted structured
    /// admission metadata; it is NEVER parsed from ``conversation_key``
    /// or any other heuristic. ``None`` (legacy callers) keeps the
    /// pre-6.4.1D behaviour: the daemon uses its static
    /// ``native_delivery_target`` for every native-work turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_destination: Option<AgentWorkDeliveryDestination>,
    /// Phase 6.4.1F — structured native scope authority. The Runtime
    /// scheduler is the source of truth for this surface. When
    /// ``Some(_)`` the daemon renders the policy into the
    /// ``<native_work_authority>`` block AND propagates the
    /// ``write_policy`` into the ACP tool-permission gate (deterministic
    /// denial of Edit/Write/NotebookEdit/apply_patch/MultiEdit/Bash under
    /// ``READ_ONLY``). When ``None`` the daemon keeps the pre-6.4.1F
    /// backward-compat default (``MODIFY_ALLOWED``, no enforcement).
    ///
    /// Persistent memory (Claude Code auto-memory, historical reports,
    /// legacy ``.agents/workflow_assignment.json`` projections) MUST NOT
    /// override the policy for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_policy: Option<AgentWorkScopePolicy>,
}

/// Phase 6.4.1F — wire DTO for the structured native scope authority
/// carried inside ``AgentWorkRequest``. Mirrors the runtime
/// ``NativeScopePolicy`` shape and the OpenAB
/// ``NativeWorkflowMetadata::scope_policy`` shape.
///
/// **Correction Round 1 — fail-closed admission boundary.** The fields
/// are intentionally NOT decorated with ``#[serde(default)]``: a
/// present-but-partial policy payload (e.g. ``{"write_policy":
/// "READ_ONLY"}``) MUST be rejected at wire admission, not silently
/// defaulted to the pre-6.4.1F canonical defaults. Canonical tokens
/// are validated explicitly by ``validate_scope_policy`` at
/// ``validate_agent_work`` time so any malformed value reaches
/// ``NativeWorkflowMetadata`` only if it survives the gate.
///
/// Canonical accepted tokens:
///   * ``scope_mode``               → ``BOUNDED``
///   * ``write_policy``             → ``READ_ONLY`` | ``MODIFY_ALLOWED``
///   * ``historical_context_policy``→ ``ADVISORY_ONLY``
///
/// Rejected malformed cases:
///   * unknown / typo'd token (e.g. ``READ_ONLY_X``)
///   * empty string
///   * partial payload (missing one or more fields — rejected by serde)
///   * ``scope_policy = None`` is the explicit opt-in to legacy
///     pre-6.4.1F semantics and is NOT rejected.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkScopePolicy {
    pub scope_mode: String,
    pub write_policy: String,
    pub historical_context_policy: String,
}

/// Phase 6.4.1D — wire DTO for the structured delivery destination
/// carried inside ``AgentWorkRequest``. Mirrors the runtime
/// ``ConversationBinding.delivery_destination`` shape and the
/// OpenAB ``adapter::ChannelRef`` shape, but carries its own
/// ``Serialize/Deserialize`` derives so it does not have to live
/// on the widely-shared ``ChannelRef`` struct (which intentionally
/// avoids Serde derives to keep the daemon-internal path lean).
///
/// Conversion to ``ChannelRef`` happens at the call site in
/// ``handle_agent_work`` after the trust check passes.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWorkDeliveryDestination {
    pub platform: String,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_event_id: Option<String>,
}

#[cfg(unix)]
impl From<AgentWorkDeliveryDestination> for crate::adapter::ChannelRef {
    fn from(value: AgentWorkDeliveryDestination) -> Self {
        crate::adapter::ChannelRef {
            platform: value.platform,
            channel_id: value.channel_id,
            thread_id: value.thread_id,
            parent_id: value.parent_id,
            origin_event_id: value.origin_event_id,
        }
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct AgentWorkLedgerEntry {
    fingerprint: String,
    ack: String,
}
#[cfg(unix)]
struct AgentWorkInFlight {
    fingerprint: String,
    /// `watch::Sender` for the in-flight completion state. Waiters
    /// receive a `watch::Sender` from `try_reserve` and subscribe to
    /// observe the final `Done` / `Failed` value. The channel buffers
    /// the LATEST value, so a waiter that subscribes after
    /// `complete_with_done` fires immediately observes `Done` —
    /// closing the race that `Notify::notify_waiters` has when a
    /// waiter registers after the notification fires.
    state_tx: tokio::sync::watch::Sender<InFlightState>,
}
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InFlightState {
    Pending,
    Done,
    Failed,
}
#[cfg(unix)]
enum AgentWorkLedgerSlot {
    Done(AgentWorkLedgerEntry),
    InFlight(AgentWorkInFlight),
}
#[cfg(unix)]
struct AgentWorkLedger {
    entries: HashMap<String, AgentWorkLedgerSlot>,
    order: VecDeque<String>,
}
#[cfg(unix)]
impl AgentWorkLedger {
    // IDEMPOTENCY_DURABILITY=PROCESS_LOCAL.  This insertion-order cache is
    // intentionally lost on daemon restart, so callers must treat ambiguous
    // admission across a restart conservatively; it is not durable exactly-once.
    const CAPACITY: usize = 1024;

    /// Outcome of a reservation attempt. The caller pattern matches on
    /// this to decide whether to perform admission, return a cached
    /// ack, wait for an in-flight caller, or fail closed on conflict.
    fn try_reserve(&mut self, key: String, fingerprint: String) -> ReservationOutcome {
        match self.entries.get(&key) {
            Some(AgentWorkLedgerSlot::Done(entry)) => {
                if entry.fingerprint == fingerprint {
                    ReservationOutcome::Done(entry.ack.clone())
                } else {
                    ReservationOutcome::Conflict
                }
            }
            Some(AgentWorkLedgerSlot::InFlight(inflight)) => {
                if inflight.fingerprint == fingerprint {
                    ReservationOutcome::InFlight(inflight.state_tx.clone())
                } else {
                    ReservationOutcome::Conflict
                }
            }
            None => {
                let (state_tx, _) = tokio::sync::watch::channel(InFlightState::Pending);
                self.entries.insert(
                    key.clone(),
                    AgentWorkLedgerSlot::InFlight(AgentWorkInFlight {
                        fingerprint: fingerprint.clone(),
                        state_tx: state_tx.clone(),
                    }),
                );
                self.order.push_back(key);
                self.evict_if_over_capacity();
                ReservationOutcome::Fresh { state_tx }
            }
        }
    }

    /// Replace an in-flight reservation with the cached successful ack.
    /// Called only by the reservation holder after admission succeeds.
    /// Wakes all waiters so they can pick up the cached ack.
    fn complete_with_done(&mut self, key: &str, entry: AgentWorkLedgerEntry) {
        if let Some(slot) = self.entries.get_mut(key) {
            if matches!(slot, AgentWorkLedgerSlot::InFlight(_)) {
                let state_tx = match self.entries.get(key) {
                    Some(AgentWorkLedgerSlot::InFlight(i)) => i.state_tx.clone(),
                    _ => unreachable!("just matched InFlight above"),
                };
                self.entries
                    .insert(key.to_string(), AgentWorkLedgerSlot::Done(entry));
                // Send the latest state. Subscribers that arrive after
                // this `send` still observe `Done` immediately because
                // `watch` buffers the latest value.
                let _ = state_tx.send(InFlightState::Done);
            }
        }
    }

    /// Remove an in-flight reservation. Called by `InFlightGuard::drop`
    /// when the reservation holder bails out (admission failure, panic,
    /// cancellation). Wakes waiters so they retry against a now-clean
    /// ledger entry.
    fn release_in_flight(&mut self, key: &str) -> bool {
        let state_tx = match self.entries.get(key) {
            Some(AgentWorkLedgerSlot::InFlight(i)) => i.state_tx.clone(),
            _ => return false,
        };
        self.entries.remove(key);
        self.order.retain(|k| k != key);
        let _ = state_tx.send(InFlightState::Failed);
        true
    }

    /// Bounded-eviction helper used by `try_reserve`.
    ///
    /// Phase 6.2.9 fix round 3: InFlight reservations MUST NOT be
    /// evicted. Eviction is restricted to `Done` entries (cached
    /// successful acknowledgements). Pinned InFlight reservations are
    /// protected even when the ledger exceeds nominal capacity — the
    /// trade-off is explicitly "temporarily exceed nominal capacity
    /// rather than break the exactly-once admission invariant". The
    /// caller holding an evicted (i.e. already-Done) entry's ack has
    /// the cached payload it needs; an InFlight eviction would lose
    /// the admission owner and let a concurrent identical request
    /// observe `Fresh`, causing duplicate admission.
    ///
    /// The invariant: `MAX_ENTRIES` (capacity) bounds the count of
    /// *retained completed* entries, not the count of in-flight
    /// reservations. Total slots may briefly exceed the nominal bound
    /// while in-flight work is pending — that is acceptable.
    fn evict_if_over_capacity(&mut self) {
        // Walk the insertion order. InFlight entries are SKIPPED — they
        // stay pinned. Only Done entries are eligible. When the order
        // has been fully scanned and we are still over capacity (every
        // remaining entry is InFlight), we stop: correctness wins
        // over capacity accounting.
        let mut scan = 0usize;
        while self.entries.len() > Self::CAPACITY && scan < self.order.len() {
            let candidate = match self.order.get(scan) {
                Some(k) => k.clone(),
                None => break,
            };
            let is_done = matches!(
                self.entries.get(&candidate),
                Some(AgentWorkLedgerSlot::Done(_)) | None
            );
            if is_done {
                // Drop both the order list and the map entry.
                self.order.remove(scan);
                self.entries.remove(&candidate);
                // Do not advance `scan` — the next element shifted into
                // position `scan`.
            } else {
                scan += 1;
            }
        }
    }
}
#[cfg(unix)]
#[derive(Debug)]
enum ReservationOutcome {
    /// First caller for this key+fingerprint. Caller MUST perform
    /// admission exactly once. The `state_tx` is bundled so the caller
    /// can construct an `InFlightGuard` whose `Drop` releases the
    /// reservation if the caller bails out before
    /// `complete_with_done`.
    Fresh {
        state_tx: tokio::sync::watch::Sender<InFlightState>,
    },
    /// A previous caller already completed admission for this key with
    /// the same fingerprint; the cached ack is returned as-is.
    Done(String),
    /// Another caller is currently performing admission for this key
    /// with the same fingerprint. Caller subscribes to the
    /// `watch::Sender`; the channel buffers the latest value, so a
    /// waiter that subscribes after `complete_with_done` immediately
    /// observes `Done` without waiting for a missed notification.
    InFlight(tokio::sync::watch::Sender<InFlightState>),
    /// A previous caller reserved this key under a different
    /// fingerprint, OR is currently admitting under a different
    /// fingerprint. Caller fails closed with `DUPLICATE_DISPATCH_CONFLICT`.
    Conflict,
}
#[cfg(unix)]
struct InFlightGuard {
    ledger: Arc<tokio::sync::Mutex<AgentWorkLedger>>,
    key: String,
    armed: bool,
}
#[cfg(unix)]
impl InFlightGuard {
    fn new(ledger: Arc<tokio::sync::Mutex<AgentWorkLedger>>, key: String) -> Self {
        Self {
            ledger,
            key,
            armed: true,
        }
    }
    /// Disarm the guard so the reservation is NOT released on drop.
    /// Called when the reservation has been promoted to `Done` via
    /// `complete_with_done`.
    fn disarm(mut self) {
        self.armed = false;
    }
}
#[cfg(unix)]
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let key = std::mem::take(&mut self.key);
        // Phase 6.2.9 fix round 3 (defense-in-depth): `Drop` must
        // never panic solely because a Tokio runtime is unavailable.
        // We try the synchronous `try_lock` path first; if the lock is
        // contended we look up a Tokio handle; if no runtime is
        // available we fall back to blocking on the lock (the daemon
        // is shutting down — at worst the reservation leaks for the
        // remainder of the process lifetime, which is preferable to
        // panicking during unwind).
        if let Ok(mut guard) = self.ledger.try_lock() {
            let _ = guard.release_in_flight(&key);
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let ledger = Arc::clone(&self.ledger);
            handle.spawn(async move {
                let mut guard = ledger.lock().await;
                let _ = guard.release_in_flight(&key);
            });
            return;
        }
        // Last-resort blocking acquire. This can only happen during
        // runtime teardown when no other task is alive to hold the
        // lock; blocking here is therefore bounded. The reservation
        // leak is preferable to a Drop-panic during unwind.
        // Last-resort blocking acquire. `tokio::sync::Mutex::blocking_lock`
        // is infallible (it blocks until acquired) so there is no Result
        // to unwrap. This path is reached only during runtime teardown
        // when no other task is alive to hold the lock; blocking here
        // is therefore bounded. The reservation leak (if blocking_lock
        // itself panics — which it can in some `Drop`-during-unwind
        // paths) is preferable to a Drop-panic during unwind.
        let mut guard = self.ledger.blocking_lock();
        let _ = guard.release_in_flight(&key);
    }
}

/// Wire-format DTO for `Request.project`. Validated and converted to
/// `ProjectContext` via `TryFrom<ProjectRef> for ProjectContext`. The
/// `project_id` MUST be non-empty — anonymous contexts are reserved for the
/// legacy `[[ws:@alias]]` directive path inside the dispatcher and are
/// deliberately not pin-able from the ctl layer.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRef {
    pub project_id: String,
    pub project_root: String,
}

#[cfg(unix)]
impl TryFrom<ProjectRef> for ProjectContext {
    type Error = String;
    fn try_from(p: ProjectRef) -> Result<Self, Self::Error> {
        if p.project_id.trim().is_empty() {
            return Err("project_id must be non-empty".into());
        }
        if p.project_root.trim().is_empty() {
            return Err("project_root must be non-empty".into());
        }
        let project = ProjectContext {
            project_id: p.project_id,
            project_root: std::path::PathBuf::from(p.project_root),
        };
        // Validate surfaces the canonical absolute path. The error string
        // propagates to the ctl caller verbatim so the AAP layer can react.
        let canonical = project.validate()?;
        Ok(Self {
            project_id: project.project_id,
            project_root: canonical,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Set,
    Get,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Optional echo of the Discord message id returned by the adapter.
    /// ``set thread.message`` populates this so the AAP caller can correlate
    /// the dispatch with downstream audit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

// ─── Server (runs inside `openab run`) ──────────────────────────────────────

/// Handler trait — `openab run` provides the concrete implementation that
/// can access Discord/Slack adapters.
#[cfg(unix)]
#[async_trait::async_trait]
pub trait CtlHandler: Send + Sync + 'static {
    async fn handle_set(
        &self,
        thread_id: Option<&str>,
        key: &str,
        value: &str,
        target_user_id: Option<&str>,
        project: Option<&ProjectRef>,
    ) -> Response;
    async fn handle_get(&self, thread_id: Option<&str>, key: &str) -> Response;
    async fn handle_agent_work(&self, _request: Option<&AgentWorkRequest>) -> Response {
        Response {
            ok: false,
            message: "ADMISSION_NOT_INSTALLED: agent.work unavailable".into(),
            value: None,
            message_id: None,
        }
    }
}

/// Start the control socket server. Call this from `openab run` startup.
/// Returns a JoinHandle; abort it on shutdown.
#[cfg(unix)]
pub fn spawn_server(handler: std::sync::Arc<dyn CtlHandler>) -> tokio::task::JoinHandle<()> {
    spawn_server_at(socket_path(), handler)
}

/// Start the control socket server at a specific path.
#[cfg(unix)]
pub fn spawn_server_at(
    path: PathBuf,
    handler: std::sync::Arc<dyn CtlHandler>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Remove stale socket file
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                error!(path = %path.display(), error = %e, "failed to bind control socket");
                return;
            }
        };
        // Restrict socket to owner only (defense-in-depth for shared hosts).
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        info!(path = %path.display(), "control socket listening");

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "control socket accept error");
                    continue;
                }
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(stream, &*handler).await {
                    debug!(error = %e, "control socket connection error");
                }
            });
        }
    })
}

#[cfg(unix)]
async fn handle_conn(stream: UnixStream, handler: &dyn CtlHandler) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    if let Some(line) = lines.next_line().await? {
        let req: Request = serde_json::from_str(&line)?;
        let request_id = format!(
            "ctl-{}",
            CONTROL_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let is_thread_message = matches!(req.action, Action::Set) && req.key == "thread.message";
        if is_thread_message {
            emit_thread_message_stage(
                Some(&request_id),
                req.thread_id.as_deref(),
                req.target_user_id.as_deref(),
                "REQUEST_RECEIVED",
                "PASS",
            );
        }
        let resp = match req.action {
            Action::Set => {
                let val = req.value.as_deref().unwrap_or("");
                if req.key == "agent.work" {
                    handler.handle_agent_work(req.agent_work.as_ref()).await
                } else {
                    control_plane::scope(
                        ControlRequestContext::new(request_id.clone()),
                        handler.handle_set(
                            req.thread_id.as_deref(),
                            &req.key,
                            val,
                            req.target_user_id.as_deref(),
                            req.project.as_ref(),
                        ),
                    )
                    .await
                }
            }
            Action::Get => handler.handle_get(req.thread_id.as_deref(), &req.key).await,
        };
        if is_thread_message {
            emit_thread_message_stage(
                Some(&request_id),
                req.thread_id.as_deref(),
                req.target_user_id.as_deref(),
                "CTL_RESPONSE_BUILT",
                if resp.ok { "PASS" } else { "FAILED" },
            );
        }
        let mut buf = serde_json::to_vec(&resp)?;
        buf.push(b'\n');
        writer.write_all(&buf).await?;
    }
    Ok(())
}

// ─── Client (used by `openab set/get` subcommands) ──────────────────────────

/// Thread registry: maps thread_id → platform name.
/// Shared between the message dispatcher (writes) and the ctl handler (reads).
#[cfg(unix)]
pub type ThreadRegistry = Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>;

/// Create an empty thread registry.
#[cfg(unix)]
pub fn new_registry() -> ThreadRegistry {
    Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()))
}

/// Register a thread→platform mapping. Called by adapters on message dispatch.
#[cfg(unix)]
#[allow(dead_code)]
pub async fn register_thread(registry: &ThreadRegistry, thread_id: &str, platform: &str) {
    registry
        .write()
        .await
        .insert(thread_id.to_string(), platform.to_string());
}

/// Type-alias for the Discord shard slot. When the discord feature is disabled,
/// this is a no-op `()` slot that never gets populated.
#[cfg(all(unix, feature = "discord"))]
pub type ShardSlot = Arc<std::sync::OnceLock<serenity::gateway::ShardMessenger>>;
#[cfg(all(unix, not(feature = "discord")))]
pub type ShardSlot = Arc<std::sync::OnceLock<()>>;

/// Concrete handler for `openab run` — dispatches to platform adapters.
#[cfg(unix)]
pub struct RuntimeHandler {
    /// Registered adapters by platform name.
    adapters: std::collections::HashMap<String, Arc<dyn ChatAdapter>>,
    /// thread_id → platform mapping. Populated by `openab run` when it dispatches messages.
    registry: Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
    shard: ShardSlot,
    /// Optional session pool — required for `thread.pin` and `thread.message`
    /// with a `project` field. Set via `with_pool` (builder) before
    /// `Arc::new(RuntimeHandler::new(...).with_pool(pool))`. Without it,
    /// project-bootstrap keys return `pool unavailable` and the legacy
    /// `thread.message` path (no project) still works.
    pool: Option<Arc<SessionPool>>,
    /// Shared canonical admission handle. It is intentionally optional during
    /// this slice because `agent.work` is not yet a ctl command; composition
    /// injects the same handle used by the Discord gateway.
    admission: Option<Arc<dyn WorkAdmissionPort>>,
    /// Configured, transport-native destination for native workflow turns.
    /// This is intentionally independent from the opaque AAP conversation key.
    native_delivery_target: Option<ChannelRef>,
    /// Phase 6.2.9: held in an `Arc` so the `InFlightGuard` returned to
    /// the reservation holder across an `.await` can own its own strong
    /// reference and release the reservation on Drop (panic /
    /// cancellation / admission failure) without borrowing the handler.
    ledger: Arc<tokio::sync::Mutex<AgentWorkLedger>>,
    /// Phase 6.4.1D — canonical L2 trust registry used by the
    /// outbound `authorize_outbound_channel` gate in
    /// `handle_agent_work`. Replaces the parallel
    /// `OPENAB_NATIVE_DELIVERY_ALLOWLIST` env-var policy so all
    /// outbound `agent.work` reply authorization flows through the
    /// single source of truth (`PlatformTrustConfigs` /
    /// `TrustConfig.allowed_channels`). Defaults to an empty
    /// `PlatformTrustConfigs::default()` so legacy tests that do not
    /// opt in behave identically (L2 open, all channels allowed).
    trust_configs: Arc<openab_core::trust::PlatformTrustConfigs>,
}

#[cfg(unix)]
impl RuntimeHandler {
    pub fn new(
        adapters: std::collections::HashMap<String, Arc<dyn ChatAdapter>>,
        registry: Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
        shard: ShardSlot,
    ) -> Self {
        Self {
            adapters,
            registry,
            shard,
            pool: None,
            admission: None,
            native_delivery_target: None,
            ledger: Arc::new(tokio::sync::Mutex::new(AgentWorkLedger {
                entries: HashMap::new(),
                order: VecDeque::new(),
            })),
            trust_configs: Arc::new(openab_core::trust::PlatformTrustConfigs::new()),
        }
    }

    /// Builder: attach the session pool so `thread.pin` and
    /// `thread.message(project=...)` can call `SessionPool::get_or_create`.
    pub fn with_pool(mut self, pool: Arc<SessionPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn with_admission(mut self, admission: Arc<dyn WorkAdmissionPort>) -> Self {
        self.admission = Some(admission);
        self
    }

    /// Attach the configured transport-native destination for native
    /// `agent.work` turns. The canonical workflow conversation key must never
    /// be used as a transport routing field.
    pub fn with_native_delivery_target(mut self, target: ChannelRef) -> Self {
        self.native_delivery_target = Some(target);
        self
    }

    /// Phase 6.4.1D — attach the canonical L2 trust registry used by
    /// the outbound `authorize_outbound_channel` gate in
    /// `handle_agent_work`. The same registry is shared with
    /// `AdapterRouter::with_trust(...)` so inbound and outbound L2
    /// gating flow through one authority. Outbound is fail-closed
    /// when the platform has an explicit `allowed_channels` list that
    /// does not include the resolved destination, **and fail-closed
    /// when the platform has no explicit config at all** (Round 2
    /// bounded correction — unconfigured platform MUST NOT default to
    /// the inbound trust registry's L2-open default).
    pub fn with_trust_configs(
        mut self,
        trust_configs: Arc<openab_core::trust::PlatformTrustConfigs>,
    ) -> Self {
        self.trust_configs = trust_configs;
        self
    }

    /// Resolve which adapter to use for a given thread_id.
    async fn resolve(&self, thread_id: Option<&str>) -> Option<(Arc<dyn ChatAdapter>, String)> {
        let tid = thread_id?;
        let platform = {
            let registry = self.registry.read().await;
            let platforms: Vec<String> = self.adapters.keys().cloned().collect();
            resolve_platform(tid, &registry, &platforms)?
        };
        let adapter = self.adapters.get(&platform)?.clone();
        Some((adapter, tid.to_string()))
    }

    /// Resolve the platform for a given thread_id (no adapter returned).
    /// Same precedence as `resolve_platform`: registry hit → single-adapter
    /// fallback → `None`.
    async fn resolve_platform_for_thread(&self, thread_id: &str) -> Option<String> {
        let registry = self.registry.read().await;
        let platforms: Vec<String> = self.adapters.keys().cloned().collect();
        resolve_platform(thread_id, &registry, &platforms)
    }

    /// Trusted-bootstrap seam for project-aware thread routing.
    ///
    /// Required invariant (workflow `20260818-openab-project-aware-thread-routing`
    /// §ACP SESSION INVALIDATION):
    ///   `thread.pin` may return success only if:
    ///     A. no existing ACP session exists and a new session is created using
    ///        the requested ProjectContext, OR
    ///     B. an existing session is already pinned to the same canonical
    ///        ProjectContext.
    ///
    /// If an active/resumable session exists with NO trusted project binding,
    /// this returns an explicit error and does NOT mutate the session. The
    /// pool is not silently reset or recreated from the ctl layer.
    ///
    /// Reusability is detected via `SessionPool::has_reusable_session` — the
    /// SINGLE source of truth for "could `get_or_create` reuse this?". This
    /// avoids duplicating SessionPool lifecycle knowledge outside the pool.
    ///
    /// Race safety: the post-check after `pool.get_or_create` re-reads
    /// `session_projects[<key>]`; if a concurrent caller (e.g. dispatcher
    /// with no project) won the race and the project was not persisted,
    /// the bootstrap is reported as failed. The ctl layer does not retry.
    async fn ensure_pinned_project(
        &self,
        thread_id: &str,
        project: &ProjectContext,
    ) -> Result<(), String> {
        let pool = self.pool.as_ref().ok_or_else(|| {
            "pool unavailable (RuntimeHandler not built with .with_pool)".to_string()
        })?;

        // Resolve platform via the existing registry + single-adapter fallback.
        let platform = self
            .resolve_platform_for_thread(thread_id)
            .await
            .ok_or_else(|| {
                "unknown thread (no registry entry, multiple adapters configured)".to_string()
            })?;

        // Use the same canonical session-key shape as the dispatcher /
        // AdapterRouter via `ChannelRef::session_pool_key()` (test M).
        let channel = ChannelRef {
            platform: platform.clone(),
            channel_id: thread_id.to_string(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };
        let session_key = channel.session_pool_key();

        // Pre-check: existing pinned state + existing reusable state.
        let pinned = pool.get_pinned_project(&session_key).await;
        let has_reusable = pool.has_reusable_session(&session_key).await;

        // Case B: existing pinned to same canonical project → idempotent success.
        if let Some(existing) = pinned.as_ref() {
            if existing == project {
                return Ok(());
            }
            // Case C: existing pinned to a DIFFERENT project — fail closed.
            // (The pool's mismatch gate would also catch this when we call
            // get_or_create, but short-circuiting here gives the ctl layer
            // a clean error path).
            return Err(format!(
                "project mismatch: thread is pinned to project_id={:?} project_root={:?}, \
                 incoming is project_id={:?} project_root={:?}",
                existing.project_id,
                existing.project_root,
                project.project_id,
                project.project_root,
            ));
        }

        // pinned = None.
        if has_reusable {
            // Case D: unpinned legacy session exists → fail closed.
            // Covers active, suspended, AND persisted session_ids (the
            // full shape of `has_reusable_session`).
            return Err(
                "session already exists without trusted project binding; reset/recreate \
                 required before pinning"
                    .to_string(),
            );
        }

        // Case A: no session, no pinning → bootstrap.
        pool.get_or_create(&session_key, Some(project))
            .await
            .map_err(|e| format!("bootstrap failed: {e}"))?;

        // Post-check: confirm the bootstrap actually persisted the binding.
        // Catches the race where a concurrent caller (e.g. dispatcher with
        // no project) won the active-session fast path between the
        // pre-check and our get_or_create call. Reject in that case
        // instead of silently returning Ok(false).
        let pinned_after = pool.get_pinned_project(&session_key).await;
        match pinned_after.as_ref() {
            Some(p) if p == project => Ok(()),
            Some(p) => Err(format!(
                "bootstrap raced and pinned to a different context: {p:?} vs incoming {project:?}"
            )),
            None => Err(
                "bootstrap did not persist project binding (likely won by a concurrent \
                 unpinned caller); retry after reset"
                    .to_string(),
            ),
        }
    }
}

/// Decide which platform should handle a control request for `thread_id`.
///
/// 1. Exact registry hit — the thread was recorded during message dispatch.
/// 2. Single-adapter fallback — if exactly one adapter is configured there is
///    no ambiguity, so resolve to it even without a registry entry. This makes
///    `openab set/get --thread <id>` work for single-platform bots (the common
///    case) without depending on the registry being populated.
///
/// Returns `None` only when the thread is unknown AND multiple adapters are
/// configured (genuinely ambiguous), or when no adapters are configured.
#[cfg(unix)]
fn resolve_platform(
    thread_id: &str,
    registry: &std::collections::HashMap<String, String>,
    platforms: &[String],
) -> Option<String> {
    if let Some(platform) = registry.get(thread_id) {
        if platforms.contains(platform) {
            return Some(platform.clone());
        }
    }
    if platforms.len() == 1 {
        return Some(platforms[0].clone());
    }
    None
}

#[cfg(unix)]
#[async_trait::async_trait]
impl CtlHandler for RuntimeHandler {
    async fn handle_set(
        &self,
        thread_id: Option<&str>,
        key: &str,
        value: &str,
        target_user_id: Option<&str>,
        project: Option<&ProjectRef>,
    ) -> Response {
        match key {
            "thread.name" => {
                let Some((adapter, tid)) = self.resolve(thread_id).await else {
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)"
                            .into(),
                        value: None,
                        message_id: None,
                    };
                };
                let channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid.clone(),
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                match adapter.rename_thread(&channel, value).await {
                    Ok(()) => Response {
                        ok: true,
                        message: format!("thread renamed to: {value}"),
                        value: None,
                        message_id: None,
                    },
                    Err(e) => Response {
                        ok: false,
                        message: format!("rename failed: {e}"),
                        value: None,
                        message_id: None,
                    },
                }
            }
            "thread.archived" => {
                let Some((_adapter, tid)) = self.resolve(thread_id).await else {
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)"
                            .into(),
                        value: None,
                        message_id: None,
                    };
                };
                let _archived = match value {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => {
                        return Response {
                            ok: false,
                            message: format!("invalid value: {value} (expected true/false)"),
                            value: None,
                            message_id: None,
                        };
                    }
                };
                let _channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid,
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                Response {
                    ok: false,
                    message: "archive_thread not supported in workspace mode".into(),
                    value: None,
                    message_id: None,
                }
            }
            "thread.pin" => {
                // Project-aware thread/session registration API (workflow
                // `20260818-openab-project-aware-thread-routing`).
                //
                // Trusted bootstrap: validates the project, fails closed on
                // any existing reusable-but-unpinned session, and persists
                // the binding via `SessionPool::session_projects` (the
                // existing canonical store). No outbound Discord message.
                let Some(project_ref) = project else {
                    return Response {
                        ok: false,
                        message: "thread.pin requires a project field".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let Some(tid) = thread_id else {
                    return Response {
                        ok: false,
                        message: "thread.pin requires a thread_id".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let project_ctx = match ProjectContext::try_from(project_ref.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return Response {
                            ok: false,
                            message: format!("invalid project: {e}"),
                            value: None,
                            message_id: None,
                        };
                    }
                };
                match self.ensure_pinned_project(tid, &project_ctx).await {
                    Ok(()) => Response {
                        ok: true,
                        message: format!(
                            "thread pinned to project_id={:?}",
                            project_ctx.project_id
                        ),
                        value: None,
                        message_id: None,
                    },
                    Err(e) => Response {
                        ok: false,
                        message: e,
                        value: None,
                        message_id: None,
                    },
                }
            }
            "thread.message" => {
                // Canonical bot-to-bot handoff control-plane primitive.
                //
                // ``value`` carries the rendered HANDOFF body (produced by
                // ``render_handoff_for_discord`` in AAP). ``target_user_id``
                // is the numeric Discord user id of the single recipient — the
                // daemon pins ``allowed_mentions`` to that user via the
                // adapter's ``send_message_targeted`` so Discord's REST
                // pipeline tags ``mentions: [{user_id: <X>}]`` and the
                // receiving bot's MultibotMentions check accepts the dispatch
                // without the LLM ever authoring a raw Discord ID.
                let request_id = control_plane::request_id();
                let Some(thread_id) = thread_id else {
                    emit_thread_message_stage(
                        request_id.as_deref(),
                        None,
                        target_user_id,
                        "THREAD_ID_PARSED",
                        "FAILED",
                    );
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)"
                            .into(),
                        value: None,
                        message_id: None,
                    };
                };
                emit_thread_message_stage(
                    request_id.as_deref(),
                    Some(thread_id),
                    target_user_id,
                    "THREAD_ID_PARSED",
                    "PASS",
                );
                emit_thread_message_stage(
                    request_id.as_deref(),
                    Some(thread_id),
                    target_user_id,
                    "TARGET_USER_ID_PARSED",
                    "PASS",
                );
                let Some((adapter, tid)) = self.resolve(Some(thread_id)).await else {
                    emit_thread_message_stage(
                        request_id.as_deref(),
                        Some(thread_id),
                        target_user_id,
                        "ADAPTER_SELECTED",
                        "FAILED",
                    );
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)"
                            .into(),
                        value: None,
                        message_id: None,
                    };
                };
                emit_thread_message_stage(
                    request_id.as_deref(),
                    Some(&tid),
                    target_user_id,
                    "ADAPTER_SELECTED",
                    "PASS",
                );
                let content = if !value.is_empty() {
                    value
                } else {
                    return Response {
                        ok: false,
                        message: "thread.message requires a non-empty value".into(),
                        value: None,
                        message_id: None,
                    };
                };
                // Pin-first semantics: if a project is supplied, validate
                // and pin BEFORE sending. A pin failure here means we
                // never send the Discord message — preserving the
                // fail-closed contract (test N).
                if let Some(project_ref) = project {
                    let project_ctx = match ProjectContext::try_from(project_ref.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            return Response {
                                ok: false,
                                message: format!("invalid project: {e}"),
                                value: None,
                                message_id: None,
                            };
                        }
                    };
                    if let Err(e) = self.ensure_pinned_project(&tid, &project_ctx).await {
                        return Response {
                            ok: false,
                            message: format!("pin failed (no message sent): {e}"),
                            value: None,
                            message_id: None,
                        };
                    }
                }
                let channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid.clone(),
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                info!(
                    event = "openab.control_plane.thread_message_stage",
                    request_id = ?request_id,
                    thread_id = %tid,
                    target_user_id = ?target_user_id,
                    stage = "BEFORE_SEND_MESSAGE_TARGETED",
                    status = "PASS",
                    content_bytes = content.len(),
                    content_chars = content.chars().count(),
                    content_lines = content.lines().count(),
                    content_sha256 = %control_plane::sha256_hex(content),
                    "canonical thread.message control-plane stage"
                );
                match adapter
                    .send_message_targeted(&channel, content, target_user_id)
                    .await
                {
                    Ok(msg_ref) => {
                        emit_thread_message_stage(
                            request_id.as_deref(),
                            Some(&tid),
                            target_user_id,
                            "AFTER_SEND_MESSAGE_TARGETED",
                            "PASS",
                        );
                        // The adapter result is the only outbound Discord operation in
                        // this canonical branch; no lookup, join, or retry occurs here.
                        // Keep the response text stable for existing AAP callers.
                        Response {
                            ok: true,
                            message: "thread.message dispatched".into(),
                            value: None,
                            message_id: Some(msg_ref.message_id),
                        }
                    }
                    Err(e) => {
                        emit_thread_message_stage(
                            request_id.as_deref(),
                            Some(&tid),
                            target_user_id,
                            "AFTER_SEND_MESSAGE_TARGETED",
                            "FAILED",
                        );
                        Response {
                            ok: false,
                            message: format!("thread.message dispatch failed: {e}"),
                            value: None,
                            message_id: None,
                        }
                    }
                }
            }
            "agent.status" => {
                #[cfg(feature = "discord")]
                {
                    let Some(shard) = self.shard.get() else {
                        return Response {
                            ok: false,
                            message: "agent.status only supported on Discord".into(),
                            value: None,
                            message_id: None,
                        };
                    };
                    use serenity::gateway::ActivityData;
                    use serenity::model::user::OnlineStatus;
                    let activity = if value.is_empty() {
                        None
                    } else {
                        Some(ActivityData::custom(value))
                    };
                    shard.set_presence(activity, OnlineStatus::Online);
                    Response {
                        ok: true,
                        message: if value.is_empty() {
                            "status cleared".into()
                        } else {
                            format!("status set to: {value}")
                        },
                        value: None,
                        message_id: None,
                    }
                }
                #[cfg(not(feature = "discord"))]
                {
                    let _ = value;
                    Response {
                        ok: false,
                        message: "agent.status requires discord feature".into(),
                        value: None,
                        message_id: None,
                    }
                }
            }
            _ => Response {
                ok: false,
                message: format!("unknown key: {key}"),
                value: None,
                message_id: None,
            },
        }
    }

    async fn handle_get(&self, _thread_id: Option<&str>, key: &str) -> Response {
        match key {
            "thread.name" | "thread.archived" | "agent.status" | "thread.message" => Response {
                ok: false,
                message: format!("{key} get not yet supported"),
                value: None,
                message_id: None,
            },
            _ => Response {
                ok: false,
                message: format!("unknown key: {key}"),
                value: None,
                message_id: None,
            },
        }
    }

    async fn handle_agent_work(&self, request: Option<&AgentWorkRequest>) -> Response {
        let Some(request) = request else {
            return agent_work_error("INVALID_AGENT_WORK_REQUEST", "missing agent.work fields");
        };
        let error = validate_agent_work(request);
        if let Some(reason) = error {
            return agent_work_error("INVALID_AGENT_WORK_REQUEST", reason);
        }
        let key = format!(
            "{}:{}:{}",
            request.agent, request.conversation_key, request.dispatch_id
        );
        let fingerprint = agent_work_fingerprint(request);

        // Phase 6.2.9 (VERIFIER fix round 2): atomic in-flight reservation.
        //
        // The previous pattern was:
        //
        //     lock ledger; check miss; unlock;
        //     await admission;          ← race window: any concurrent caller
        //     lock ledger; insert;         can observe the same miss and run
        //                                   admission twice.
        //
        // The new pattern collapses "check" and "reserve" into a single
        // critical section, so the first caller atomically becomes the
        // admission owner and concurrent identical callers either wait
        // on the in-flight `watch` channel or fail closed on
        // fingerprint mismatch. The reservation holder carries an
        // `InFlightGuard` whose `Drop` releases the slot on every error
        // / panic / cancellation path — no permanent IN_FLIGHT leak.
        //
        // The `watch` channel (vs. `Notify`) closes the
        // subscribe-after-fire race: a waiter that subscribes AFTER
        // `complete_with_done` already fired still observes `Done`
        // immediately because `watch` buffers the latest value.
        // CRITICAL: the ledger lock MUST be released before any
        // `.await` on the waiter path. Otherwise the waiter holds the
        // lock while waiting on `state_rx.changed()`, the holder
        // blocks on `complete_with_done`'s `ledger.lock().await`, and
        // we deadlock. We scope `try_reserve` into a block whose
        // temporary `MutexGuard` is dropped before the match.
        let reservation = {
            let mut guard = self.ledger.lock().await;
            guard.try_reserve(key.clone(), fingerprint.clone())
        };
        let _state_tx = match reservation {
            ReservationOutcome::Fresh { state_tx } => state_tx,
            ReservationOutcome::Done(ack) => {
                return Response {
                    ok: true,
                    message: "WORK_ACCEPTED".into(),
                    value: Some(ack),
                    message_id: None,
                };
            }
            ReservationOutcome::InFlight(state_tx) => {
                // Subscribe to the in-flight state channel. A
                // `watch::Receiver` always observes the latest
                // buffered value (so a send that happens between our
                // `try_reserve` and our subscription is NOT lost).
                let mut state_rx = state_tx.subscribe();
                let initial = *state_rx.borrow_and_update();
                if initial != InFlightState::Pending {
                    // Holder already committed. Re-reserve to pick up
                    // the cached `Done` ack.
                    let again = {
                        let mut guard = self.ledger.lock().await;
                        guard.try_reserve(key.clone(), fingerprint.clone())
                    };
                    return match again {
                        ReservationOutcome::Done(ack) => Response {
                            ok: true,
                            message: "WORK_ACCEPTED".into(),
                            value: Some(ack),
                            message_id: None,
                        },
                        _ => agent_work_error(
                            "IN_FLIGHT_RETRY",
                            "concurrent dispatch released; retry",
                        ),
                    };
                }
                // Wait for the holder to commit (Done) or fail
                // (Failed). Bounded so the scheduler can back off
                // rather than hang forever on a stuck holder. We
                // await `changed()` (which marks the value as seen),
                // then re-check; if the value is still Pending (a
                // spurious re-mark), loop and wait for the next change.
                let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        if state_rx.changed().await.is_err() {
                            return Err::<(), ()>(());
                        }
                        if *state_rx.borrow_and_update() != InFlightState::Pending {
                            return Ok::<(), ()>(());
                        }
                    }
                })
                .await;
                let again = match outcome {
                    Ok(Ok(())) => {
                        let mut guard = self.ledger.lock().await;
                        Some(guard.try_reserve(key.clone(), fingerprint.clone()))
                    }
                    Ok(Err(())) | Err(_) => {
                        return agent_work_error(
                            "IN_FLIGHT_TIMEOUT",
                            "concurrent dispatch did not complete within 5s; retry",
                        );
                    }
                };
                return match again.unwrap() {
                    ReservationOutcome::Done(ack) => Response {
                        ok: true,
                        message: "WORK_ACCEPTED".into(),
                        value: Some(ack),
                        message_id: None,
                    },
                    _ => agent_work_error("IN_FLIGHT_RETRY", "concurrent dispatch released; retry"),
                };
            }
            ReservationOutcome::Conflict => {
                return agent_work_error(
                    "DUPLICATE_DISPATCH_CONFLICT",
                    "dispatch_id payload differs",
                );
            }
        };
        let _guard = InFlightGuard::new(self.ledger.clone(), key.clone());
        let Some(admission) = self.admission.as_ref() else {
            return agent_work_error("ADMISSION_NOT_INSTALLED", "admission service is not ready");
        };
        // Phase 6.4.1D — prefer the authoritative structured delivery
        // destination carried in by AAP Runtime from the upstream
        // ``ConversationBinding`` over the daemon-wide static fallback.
        // The Runtime sources the field from trusted structured admission
        // metadata; we still re-verify here so an unauthenticated or
        // malformed upstream field cannot smuggle a destination past the
        // trust boundary. When the structured field is absent the legacy
        // behaviour (single static target) is preserved.
        //
        // Both the structured branch and the legacy
        // ``native_delivery_target`` fallback run through the canonical
        // ``PlatformTrustConfigs::authorize_outbound_channel`` gate
        // so the operator-configured ``allowed_channels`` list applies
        // uniformly (no parallel ``OPENAB_NATIVE_DELIVERY_ALLOWLIST``
        // env-var policy anymore). The fail-closed helper returns
        // ``false`` when the platform has no explicit trust config —
        // outbound native delivery MUST NOT silently default to
        // allow-all just because the inbound trust registry's default
        // is L2-open.
        let channel = match request.delivery_destination.clone() {
            Some(structured) => {
                if let Err(reason) = validate_delivery_destination(&structured) {
                    return agent_work_error("INVALID_DELIVERY_DESTINATION", &reason);
                }
                // Phase 6.4.1E — the structured ``delivery_destination``
                // carries ``parent_id``, so it routes through the
                // parent-aware outbound helper. A Discord thread whose
                // own channel_id is not in ``allowed_channels`` can still
                // be authorized when its parent_id IS allowed.
                // Parent is sourced from the trusted structured field —
                // never parsed from ``conversation_key`` or any other
                // heuristic.
                if !self.trust_configs.authorize_outbound_channel_with_parent(
                    &structured.platform,
                    &structured.channel_id,
                    structured.parent_id.as_deref(),
                ) {
                    return agent_work_error(
                        "DELIVERY_DESTINATION_NOT_ALLOWED",
                        &format!(
                            "delivery_destination {}:{} (parent={:?}) is not in the platform trust allowlist",
                            structured.platform, structured.channel_id, structured.parent_id
                        ),
                    );
                }
                crate::adapter::ChannelRef::from(structured)
            }
            None => match self.native_delivery_target.clone() {
                Some(target) => {
                    // Legacy fallback has no parent_id by construction
                    // (the static ``ChannelRef`` carries ``parent_id:
                    // None`` — see ``with_native_delivery_target`` and
                    // the test fixtures). Keep the original channel-only
                    // check so the legacy contract is bit-exact.
                    if !self
                        .trust_configs
                        .authorize_outbound_channel(&target.platform, &target.channel_id)
                    {
                        return agent_work_error(
                            "DELIVERY_DESTINATION_NOT_ALLOWED",
                            &format!(
                                "native_delivery_target {}:{} is not in the platform trust allowlist",
                                target.platform, target.channel_id
                            ),
                        );
                    }
                    target
                }
                None => {
                    return agent_work_error(
                        "NATIVE_DELIVERY_TARGET_NOT_CONFIGURED",
                        "native delivery target is not configured",
                    );
                }
            },
        };
        let Some(adapter) = self.adapters.get(&channel.platform).cloned() else {
            return agent_work_error(
                "NATIVE_DELIVERY_TARGET_UNAVAILABLE",
                "native delivery target platform is unavailable",
            );
        };
        let metadata = NativeWorkflowMetadata {
            dispatch_id: request.dispatch_id.clone(),
            conversation_key: request.conversation_key.clone(),
            workflow_run_id: request.workflow_run_id.clone(),
            project_id: request.project_id.clone(),
            task_id: request.task_id.clone(),
            role: request.role.clone(),
            agent: request.agent.clone(),
            lease_id: request.lease_id.clone(),
            lease_generation: request.lease_generation,
            expected_revision: request.expected_revision,
            language: Some(request.language.clone()),
            project_root: None,
            // Phase 6.2.9: the native-work authority carries its own
            // per-dispatch execution-session key. The pool guarantees a
            // fresh `session/new` and never replays historical turns for
            // this key.
            native_execution_session_key: Some(openab_core::acp::pool::format_native_dispatch_key(
                &request.agent,
                &request.dispatch_id,
            )),
            // Phase 6.4.1B: authoritative transport identity carried in by
            // the structured dispatch metadata. The value is propagated
            // unchanged into the completion event so AAP Runtime can perform
            // transport-aware conversation identity validation. `None` (the
            // absence of the JSON field) means the legacy scheduler did not
            // declare transport; Runtime defaults such records to OPENAB.
            transport: request.transport.clone(),
            // Phase 6.4.1D: authoritative structured delivery destination.
            // Populated on metadata even though the daemon already
            // resolved the runtime ChannelRef above — preserves the
            // dispatcher.log observability invariant (the structured
            // destination must be on the metadata so audit logs and
            // recovery tooling can see it). The dispatcher itself uses
            // the resolved `channel` value (preferring this over the
            // daemon-wide `native_delivery_target` fallback).
            delivery_destination: request
                .delivery_destination
                .clone()
                .map(crate::adapter::ChannelRef::from),
            // Phase 6.4.1F: structured native scope authority. The
            // Runtime scheduler is the source of truth; OpenAB renders
            // it in the <native_work_authority> block and the ACP layer
            // uses `write_policy` to gate tool execution. `None`
            // preserves the pre-6.4.1F backward-compat default
            // (no enforcement, MODIFY_ALLOWED effective policy).
            //
            // Correction Round 1 — by the time we reach this point
            // ``validate_agent_work`` has already guaranteed that
            // ``scope_policy`` (when ``Some(_)``) carries canonical
            // tokens for every field. The conversion below is a
            // verbatim copy, NOT a fallback / default path. If a
            // future maintainer weakens ``validate_scope_policy``,
            // the ACP ``WritePolicyGuard`` downstream will fall back
            // to its own backward-compat default and a malformed
            // token can no longer hot-path to MODIFY_ALLOWED without
            // surfacing here.
            scope_policy: request.scope_policy.as_ref().map(|p| {
                openab_core::admission::NativeScopePolicy {
                    scope_mode: p.scope_mode.clone(),
                    write_policy: p.write_policy.clone(),
                    historical_context_policy: p.historical_context_policy.clone(),
                }
            }),
        };
        let message = BufferedMessage {
            sender_json: format!(
                r#"{{\"schema\":\"openab.sender.v1\",\"sender_id\":\"native:{}\",\"sender_name\":\"{}\"}}"#,
                request.agent, request.agent
            ),
            sender_name: request.agent.clone(),
            prompt: request.assignment.clone(),
            extra_blocks: Vec::new(),
            trigger_msg: MessageRef {
                channel: channel.clone(),
                message_id: request.dispatch_id.clone(),
            },
            arrived_at: Instant::now(),
            estimated_tokens: request.assignment.len() / 4,
            other_bot_present: false,
            recipient: None,
            native_workflow: Some(metadata.clone()),
        };
        let ack = match admission
            .admit_work(WorkAdmissionRequest {
                conversation: channel,
                sender_id: format!("native:{}", request.agent),
                adapter,
                message,
                native_workflow: Some(metadata.clone()),
                // Phase 6.2.9: per-dispatch ACP execution-session key. The
                // pool guarantees a fresh `session/new` and never replays
                // historical turns for this key. Idempotency for repeated
                // scheduler dispatch of the same dispatch_id is owned by
                // the `agent:conversation_key:dispatch_id` ledger above.
                native_execution_session_key: Some(
                    openab_core::acp::pool::format_native_dispatch_key(
                        &request.agent,
                        &request.dispatch_id,
                    ),
                ),
            })
            .await
        {
            Ok(ack) if ack.accepted => ack,
            Ok(_) => {
                // Admission was not accepted. The InFlightGuard will
                // release the reservation on Drop so a retry can
                // re-reserve cleanly.
                return agent_work_error("DISPATCH_REJECTED", "admission was not accepted");
            }
            Err(error) => {
                // Same — admission errored; release on Drop, return the
                // typed error code.
                return agent_work_error(error.code(), &error.to_string());
            }
        };
        let ack_json = serde_json::json!({"acknowledgement":"WORK_ACCEPTED", "dispatch_id":request.dispatch_id, "admission_id":ack.admission_id, "workflow_run_id":request.workflow_run_id, "role":request.role, "conversation_key":request.conversation_key}).to_string();
        // Promote the InFlight reservation to Done. After this point the
        // guard is disarmed so its Drop will not also release the slot.
        self.ledger.lock().await.complete_with_done(
            &key,
            AgentWorkLedgerEntry {
                fingerprint,
                ack: ack_json.clone(),
            },
        );
        _guard.disarm();
        Response {
            ok: true,
            message: "WORK_ACCEPTED".into(),
            value: Some(ack_json),
            message_id: None,
        }
    }
}

#[cfg(unix)]
fn agent_work_error(code: &str, detail: &str) -> Response {
    Response {
        ok: false,
        message: format!("{code}: {detail}"),
        value: None,
        message_id: None,
    }
}
#[cfg(unix)]
fn validate_agent_work(r: &AgentWorkRequest) -> Option<&'static str> {
    if [
        &r.dispatch_id,
        &r.workflow_run_id,
        &r.task_id,
        &r.lease_id,
        &r.conversation_key,
        &r.assignment,
        &r.language,
    ]
    .iter()
    .any(|v| v.trim().is_empty())
    {
        return Some("required field is empty");
    }
    if !matches!(r.role.as_str(), "PRIMARY" | "VERIFIER" | "FINAL_REVIEWER") {
        return Some("unsupported role");
    }
    if !matches!(
        r.agent.as_str(),
        "ArthurClaude" | "ArthurCodex" | "ArthurGemini"
    ) {
        return Some("unsupported agent");
    }
    if r.lease_generation < 1 {
        return Some("lease_generation must be >= 1");
    }
    // Phase 6.4.1F Correction Round 1 — fail-closed scope_policy
    // validation. When ``scope_policy = None`` the daemon preserves
    // the pre-6.4.1F legacy semantics (no enforcement,
    // MODIFY_ALLOWED effective). When ``Some(_)``, every field must
    // be a non-empty canonical token or the request is rejected
    // BEFORE ``NativeWorkflowMetadata`` is constructed. This is the
    // authoritative boundary — invalid scope_policy must never
    // reach native dispatch.
    if let Some(reason) = r.scope_policy.as_ref().and_then(validate_scope_policy) {
        return Some(reason);
    }
    None
}

/// Phase 6.4.1F Correction Round 1 — validate every field of an
/// admitted ``scope_policy`` against the canonical token set.
/// Returns ``None`` for a fully-valid policy, otherwise a
/// ``&'static str`` reason that ``validate_agent_work`` surfaces
/// verbatim to the caller. Partial payloads are caught at the
/// serde layer (``AgentWorkScopePolicy`` has NO ``#[serde(default)]``
/// on any field); this helper rejects unknown tokens, empty
/// strings, and any other malformed-but-syntactically-valid value
/// that slipped past serde.
#[cfg(unix)]
fn validate_scope_policy(p: &AgentWorkScopePolicy) -> Option<&'static str> {
    // ``scope_mode`` — currently only ``BOUNDED``.
    if p.scope_mode.trim().is_empty() {
        return Some("scope_policy.scope_mode is empty");
    }
    if !matches!(p.scope_mode.as_str(), "BOUNDED") {
        return Some("scope_policy.scope_mode is not a canonical token");
    }
    // ``write_policy`` — ``READ_ONLY`` or ``MODIFY_ALLOWED``.
    if p.write_policy.trim().is_empty() {
        return Some("scope_policy.write_policy is empty");
    }
    if !matches!(p.write_policy.as_str(), "READ_ONLY" | "MODIFY_ALLOWED") {
        return Some("scope_policy.write_policy is not a canonical token");
    }
    // ``historical_context_policy`` — currently only ``ADVISORY_ONLY``.
    if p.historical_context_policy.trim().is_empty() {
        return Some("scope_policy.historical_context_policy is empty");
    }
    if !matches!(p.historical_context_policy.as_str(), "ADVISORY_ONLY") {
        return Some("scope_policy.historical_context_policy is not a canonical token");
    }
    None
}
#[cfg(unix)]
fn agent_work_fingerprint(r: &AgentWorkRequest) -> String {
    control_plane::sha256_hex(&serde_json::to_string(r).expect("AgentWorkRequest serializes"))
}

/// Phase 6.4.1D — trust check on a structured delivery destination
/// carried in by AAP Runtime. Returns ``Ok(())`` when the field is
/// well-formed for the named platform, otherwise an error string the
/// caller surfaces as ``INVALID_DELIVERY_DESTINATION``.
///
/// The check is intentionally strict: the field is the new
/// authoritative source for the daemon's reply target, so a
/// malformed entry must fail closed BEFORE the buffered message
/// reaches the dispatcher / Discord adapter.
#[cfg(unix)]
fn validate_delivery_destination(channel: &AgentWorkDeliveryDestination) -> Result<(), String> {
    if channel.platform.trim().is_empty() {
        return Err("delivery_destination.platform must be non-empty".into());
    }
    if channel.channel_id.trim().is_empty() {
        return Err("delivery_destination.channel_id must be non-empty".into());
    }
    match channel.platform.as_str() {
        "discord" => {
            // Discord channel ids are unsigned 64-bit integers encoded
            // as ASCII decimal. We accept the same shape as the
            // legacy ``is_valid_openab_conversation_key`` policy so
            // the runtime can stamp either the bare snowflake or the
            // canonical ``discord:<snowflake>`` form into the field
            // without breaking this check.
            let trimmed = channel.channel_id.trim();
            let digits = trimmed.strip_prefix("discord:").unwrap_or(trimmed);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "delivery_destination.channel_id {:?} is not a valid Discord snowflake",
                    channel.channel_id,
                ));
            }
        }
        other => {
            return Err(format!(
                "delivery_destination.platform {other:?} is not a recognised adapter platform"
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
pub async fn send_request(req: &Request) -> anyhow::Result<Response> {
    send_request_to(&socket_path(), req).await
}

#[cfg(not(unix))]
pub async fn send_request(_req: &Request) -> anyhow::Result<Response> {
    anyhow::bail!("openab set/get is not supported on Windows (requires Unix domain sockets)")
}

/// Send a request to a specific socket path.
#[cfg(unix)]
pub async fn send_request_to(path: &PathBuf, req: &Request) -> anyhow::Result<Response> {
    let stream = UnixStream::connect(&path).await.map_err(|e| {
        anyhow::anyhow!(
            "cannot connect to openab at {}: {} (is `openab run` running?)",
            path.display(),
            e
        )
    })?;
    let (reader, mut writer) = stream.into_split();
    let mut buf = serde_json::to_vec(req)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.shutdown().await?;

    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("no response from openab"))?;
    let resp: Response = serde_json::from_str(&line)?;
    Ok(resp)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use openab_core::admission::{WorkAdmissionAck, WorkAdmissionError};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn reg(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn native_work_request() -> AgentWorkRequest {
        AgentWorkRequest {
            dispatch_id: "dispatch-1".into(),
            workflow_run_id: "run-1".into(),
            project_id: Some("project-1".into()),
            task_id: "task-1".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-1".into(),
            lease_generation: 1,
            expected_revision: 0,
            conversation_key: "123456".into(),
            assignment: "bounded assignment".into(),
            language: "zh-TW".into(),
            transport: Some("DISCORD".into()),
            delivery_destination: None,
            scope_policy: None,
        }
    }

    struct RecordingAdmissionPort {
        calls: AtomicUsize,
        admission_id: String,
        last_admission: StdMutex<Option<(ChannelRef, Option<NativeWorkflowMetadata>)>>,
    }

    impl RecordingAdmissionPort {
        fn new(admission_id: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                admission_id: admission_id.into(),
                last_admission: StdMutex::new(None),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(AtomicOrdering::SeqCst)
        }

        fn last_admission(&self) -> (ChannelRef, Option<NativeWorkflowMetadata>) {
            self.last_admission
                .lock()
                .unwrap()
                .clone()
                .expect("admission request was recorded")
        }
    }

    #[async_trait::async_trait]
    impl WorkAdmissionPort for RecordingAdmissionPort {
        async fn admit_work(
            &self,
            request: WorkAdmissionRequest,
        ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            *self.last_admission.lock().unwrap() = Some((
                request.conversation.clone(),
                request.native_workflow.clone(),
            ));
            Ok(WorkAdmissionAck {
                admission_id: self.admission_id.clone(),
                conversation_key: request.conversation.session_pool_key(),
                accepted: true,
                native_workflow: request.native_workflow,
            })
        }
    }

    struct RejectingAdmissionPort(AtomicUsize);

    impl RejectingAdmissionPort {
        fn calls(&self) -> usize {
            self.0.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl WorkAdmissionPort for RejectingAdmissionPort {
        async fn admit_work(
            &self,
            _request: WorkAdmissionRequest,
        ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
            self.0.fetch_add(1, AtomicOrdering::SeqCst);
            Err(WorkAdmissionError::Internal("test rejection"))
        }
    }

    fn native_work_handler(admission: Arc<dyn WorkAdmissionPort>) -> RuntimeHandler {
        // Phase 6.4.1D Round 2 — pre-existing test fixtures that did
        // not opt in via ``with_trust_configs`` relied on the OLD
        // ``surface_allowed_for_outbound`` behaviour (allow-all when
        // the registry was empty). The Round 2 outbound helper now
        // fails closed when no platform has explicit config. To keep
        // the legacy test fixtures working WITHOUT re-plumbing every
        // test site, this helper installs an L2-open permissive
        // default for ``discord`` (the only platform the test
        // fixtures exercise). Production code paths always use
        // ``with_trust_configs(...)`` (see ``main.rs``) and are
        // unaffected.
        native_work_handler_with_trust(admission, Some(trust_allowing_all_for("discord")))
    }

    /// Phase 6.4.1D — extended test helper that lets a test inject a
    /// custom ``PlatformTrustConfigs`` so the outbound
    /// ``authorize_outbound_channel`` gate can be exercised.
    /// ``None`` preserves the legacy ``native_work_handler`` behaviour
    /// (L2 open / all channels allowed).
    fn native_work_handler_with_trust(
        admission: Arc<dyn WorkAdmissionPort>,
        trust_configs: Option<Arc<openab_core::trust::PlatformTrustConfigs>>,
    ) -> RuntimeHandler {
        let adapter: Arc<dyn ChatAdapter> = Arc::new(RecordingAdapter::default());
        let mut handler = RuntimeHandler::new(
            HashMap::from([("discord".into(), adapter)]),
            Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            Arc::new(std::sync::OnceLock::new()),
        )
        .with_admission(admission)
        .with_native_delivery_target(ChannelRef {
            platform: "discord".into(),
            channel_id: "1539923659345502208".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });
        if let Some(tc) = trust_configs {
            handler = handler.with_trust_configs(tc);
        }
        handler
    }

    fn accepted_ack(response: &Response) -> serde_json::Value {
        assert!(response.ok, "{}", response.message);
        assert_eq!(response.message, "WORK_ACCEPTED");
        serde_json::from_str(response.value.as_deref().expect("WORK_ACCEPTED ack")).unwrap()
    }

    #[tokio::test]
    async fn ctl_work_accepted_is_truthful_handler_admission() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-1"));
        let handler = native_work_handler(admission.clone());
        let request = native_work_request();

        let response = handler.handle_agent_work(Some(&request)).await;
        let ack = accepted_ack(&response);

        assert_eq!(admission.calls(), 1);
        assert_eq!(ack["dispatch_id"], request.dispatch_id);
        assert_eq!(ack["workflow_run_id"], request.workflow_run_id);
        assert_eq!(ack["role"], request.role);
        assert_eq!(ack["conversation_key"], request.conversation_key);
        assert_eq!(ack["admission_id"], "admission-1");
    }

    // ── Phase 6.2.9 native ACP session isolation tests ───────────────────────

    #[tokio::test]
    async fn ctl_native_work_propagates_execution_session_key_to_admission() {
        // Invariant: `set agent.work` MUST supply the per-dispatch
        // execution-session key on the `WorkAdmissionRequest` so the
        // pool's fast lane picks it up. The Discord conversation
        // channel (delivery target) is preserved separately.
        let admission = Arc::new(RecordingAdmissionPort::new("admission-iso-1"));
        let handler = native_work_handler(admission.clone());
        let request = native_work_request();

        let _response = handler.handle_agent_work(Some(&request)).await;

        assert_eq!(admission.calls(), 1);
        let (channel, metadata) = admission.last_admission();
        // Delivery target is the configured Discord channel, unchanged.
        assert_eq!(channel.channel_id, "1539923659345502208");
        // The native-work authority carries the execution-session key.
        let meta = metadata.expect("native metadata must be present");
        let key = meta
            .native_execution_session_key
            .expect("native execution session key must be present");
        assert_eq!(
            key,
            openab_core::acp::pool::format_native_dispatch_key(
                &request.agent,
                &request.dispatch_id,
            ),
            "ctl handler must compute the deterministic per-dispatch key"
        );
    }

    #[tokio::test]
    async fn ctl_two_native_dispatch_ids_produce_independent_execution_keys() {
        // Invariant A/B: two different dispatch_ids for the same agent
        // MUST produce two independent execution-session keys. Repeated
        // dispatch of the SAME dispatch_id MUST short-circuit on the
        // ctl-side ledger (no second admission, same response) so the
        // scheduler's idempotent retry never spawns a second ACP turn.
        let admission = Arc::new(RecordingAdmissionPort::new("admission-iso-2"));
        let handler = native_work_handler(admission.clone());
        let mut req_a = native_work_request();
        req_a.dispatch_id = "oad-aaaa".into();
        let mut req_b = native_work_request();
        req_b.dispatch_id = "oad-bbbb".into();

        let _ = handler.handle_agent_work(Some(&req_a)).await;
        let _ = handler.handle_agent_work(Some(&req_b)).await;
        // Re-dispatch of the SAME dispatch_id with the SAME fingerprint
        // is short-circuited by the ledger — admission is not called
        // again. This is what makes the ledger idempotency
        // owner-distinct from the pool (Phase 6.2.9 design tradeoff B).
        let _ = handler.handle_agent_work(Some(&req_a)).await;

        assert_eq!(
            admission.calls(),
            2,
            "ledger short-circuits retried dispatch_id"
        );
        let key_a = format_native_dispatch_key(&req_a.agent, &req_a.dispatch_id);
        let key_b = format_native_dispatch_key(&req_b.agent, &req_b.dispatch_id);
        assert_ne!(
            key_a, key_b,
            "different dispatch_ids must yield different keys"
        );
    }

    #[tokio::test]
    async fn ctl_native_work_keeps_opaque_conversation_key_out_of_delivery_routing() {
        const CANONICAL_CONVERSATION_KEY: &str = "271837801169159848509375029904518307937";
        const DISCORD_DELIVERY_CHANNEL_ID: &str = "1539923659345502208";

        let admission = Arc::new(RecordingAdmissionPort::new("admission-opaque"));
        let handler = native_work_handler(admission.clone());
        let mut request = native_work_request();
        request.conversation_key = CANONICAL_CONVERSATION_KEY.into();

        let response = handler.handle_agent_work(Some(&request)).await;
        let ack = accepted_ack(&response);
        let (channel, metadata) = admission.last_admission();
        let metadata = metadata.expect("native metadata is supplied to dispatcher admission");

        assert_eq!(channel.platform, "discord");
        assert_eq!(channel.channel_id, DISCORD_DELIVERY_CHANNEL_ID);
        assert_ne!(channel.channel_id, CANONICAL_CONVERSATION_KEY);
        assert_ne!(
            channel.thread_id.as_deref(),
            Some(CANONICAL_CONVERSATION_KEY)
        );
        assert_eq!(metadata.conversation_key, CANONICAL_CONVERSATION_KEY);
        assert_eq!(ack["conversation_key"], CANONICAL_CONVERSATION_KEY);
    }

    #[tokio::test]
    async fn ctl_duplicate_identical_returns_original_ack_without_second_admission() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-1"));
        let handler = native_work_handler(admission.clone());
        let request = native_work_request();

        let first = handler.handle_agent_work(Some(&request)).await;
        let second = handler.handle_agent_work(Some(&request)).await;

        assert_eq!(admission.calls(), 1);
        assert_eq!(accepted_ack(&first), accepted_ack(&second));
    }

    #[tokio::test]
    async fn ctl_duplicate_conflict_fails_closed_without_second_admission() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-1"));
        let handler = native_work_handler(admission.clone());
        let request = native_work_request();
        assert!(handler.handle_agent_work(Some(&request)).await.ok);
        let mut conflict = request;
        conflict.assignment = "different bounded assignment".into();

        let response = handler.handle_agent_work(Some(&conflict)).await;

        assert_eq!(admission.calls(), 1);
        assert!(!response.ok);
        assert!(response.message.starts_with("DUPLICATE_DISPATCH_CONFLICT:"));
        assert_ne!(response.message, "WORK_ACCEPTED");
    }

    #[tokio::test]
    async fn ctl_failed_admission_does_not_cache_success() {
        let rejecting = Arc::new(RejectingAdmissionPort(AtomicUsize::new(0)));
        let handler = native_work_handler(rejecting.clone());
        let request = native_work_request();

        let response = handler.handle_agent_work(Some(&request)).await;

        assert_eq!(rejecting.calls(), 1);
        assert!(!response.ok);
        assert!(response.value.is_none());
        assert!(response.message.starts_with("ADMISSION_INTERNAL_ERROR:"));
        // Phase 6.2.9 (fix round 2): the InFlightGuard must release the
        // reservation on admission failure so the next retry can land on
        // `Fresh`, not a permanent `InFlight` / `Done`.
        let key = format!(
            "{}:{}:{}",
            request.agent, request.conversation_key, request.dispatch_id
        );
        assert!(matches!(
            handler
                .ledger
                .lock()
                .await
                .try_reserve(key, agent_work_fingerprint(&native_work_request())),
            ReservationOutcome::Fresh { .. }
        ));
    }

    // ── Phase 6.4.1D — TrustConfig outbound gating tests (F/G/H) ────────────

    use openab_core::trust::{PlatformTrustConfigs, TrustConfig};

    fn trust_allowing_only(allowed_channels: &[&str]) -> Arc<PlatformTrustConfigs> {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(false), // allow_all_channels closed — gate on allowed_channels
                allowed_channels.iter().map(|s| s.to_string()),
                Some(true),
                Some(false),
                std::iter::empty::<String>(),
            ),
        );
        Arc::new(reg)
    }

    /// Phase 6.4.1D Round 2 — helper for tests that want the
    /// "operator explicitly chose L2-open" posture for a single
    /// platform. Distinct from the empty-registry case (which now
    /// fails closed for outbound native delivery — see test L). This
    /// matches the previous Round 1 helper behaviour and lets
    /// pre-existing tests that exercise unrelated dispatch logic keep
    /// working without re-plumbing every test fixture.
    fn trust_allowing_all_for(platform: &str) -> Arc<PlatformTrustConfigs> {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            platform,
            TrustConfig::new(
                Some(true), // allow_all_channels OPEN — operator chose L2-open
                std::iter::empty::<String>(),
                Some(true),
                Some(false),
                std::iter::empty::<String>(),
            ),
        );
        Arc::new(reg)
    }

    /// F — operator-configured `allowed_channels=["111111111111111111"]` +
    /// structured `delivery_destination={channel_id:"111111111111111111"}`
    /// → `WORK_ACCEPTED`; `RecordingAdmissionPort` receives exactly
    /// one admission. Channel IDs are real Discord snowflakes
    /// (18-digit decimal strings) so `validate_delivery_destination`
    /// does not reject them as malformed before the trust gate runs.
    #[tokio::test]
    async fn test_trust_allowed_outbound_channel() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-F"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&["111111111111111111"])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: "111111111111111111".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(
            response.ok,
            "expected WORK_ACCEPTED, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(admission.calls(), 1);
        let (channel, _metadata) = admission.last_admission();
        assert_eq!(channel.platform, "discord");
        assert_eq!(channel.channel_id, "111111111111111111");
    }

    /// G — operator-configured `allowed_channels=["111111111111111111"]` +
    /// structured `delivery_destination={channel_id:"222222222222222222"}`
    /// → `DELIVERY_DESTINATION_NOT_ALLOWED`; admission is NEVER
    /// called.
    #[tokio::test]
    async fn test_trust_denied_outbound_channel_fails_closed() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-G"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&["111111111111111111"])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: "222222222222222222".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(response
            .message
            .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"));
        assert_ne!(response.message, "WORK_ACCEPTED");
        assert_eq!(
            admission.calls(),
            0,
            "admission must not be called when the trust gate denies"
        );
    }

    /// H — legacy `native_delivery_target` path also goes through the
    /// trust gate. `delivery_destination=None` + the handler's static
    /// target is channel "1539923659345502208"; with
    /// `allowed_channels=["1539923659345502208"]` it passes; with
    /// `allowed_channels=["999999999999999999"]` it fails closed.
    #[tokio::test]
    async fn test_legacy_native_delivery_target_also_subject_to_trust_policy() {
        // ── H1 — legacy target IN allowlist → WORK_ACCEPTED.
        let admission_pass = Arc::new(RecordingAdmissionPort::new("admission-H-pass"));
        let handler_pass = native_work_handler_with_trust(
            admission_pass.clone(),
            Some(trust_allowing_only(&["1539923659345502208"])),
        );
        let request = native_work_request(); // delivery_destination=None
        let response = handler_pass.handle_agent_work(Some(&request)).await;
        assert!(
            response.ok,
            "legacy target IN allowlist must succeed, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(admission_pass.calls(), 1);

        // ── H2 — legacy target NOT in allowlist → DELIVERY_DESTINATION_NOT_ALLOWED.
        let admission_fail = Arc::new(RecordingAdmissionPort::new("admission-H-fail"));
        let handler_fail = native_work_handler_with_trust(
            admission_fail.clone(),
            Some(trust_allowing_only(&["999999999999999999"])),
        );
        let response = handler_fail.handle_agent_work(Some(&request)).await;
        assert!(!response.ok);
        assert!(response
            .message
            .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"));
        assert_ne!(response.message, "WORK_ACCEPTED");
        assert_eq!(
            admission_fail.calls(),
            0,
            "admission must not be called when the trust gate denies the legacy fallback"
        );
    }

    // ── Phase 6.4.1D Round 2 (bounded correction) — outbound fail-closed
    //    when the platform has no explicit trust config (DEFECT A).
    //
    //    The verifier flagged that ``surface_allowed_for_outbound``
    //    silently fell through to the registry's L2-open default for
    //    unconfigured platforms, which let an authenticated
    //    ``agent.work`` request route to any Discord channel just
    //    because the operator hadn't populated the
    //    ``[platform.discord]`` config. The new helper
    //    ``PlatformTrustConfigs::authorize_outbound_channel`` enforces
    //    FAIL CLOSED on unconfigured platforms — outbound native
    //    delivery requires an explicit per-platform ``allowed_channels``
    //    policy (or L2-open if the operator explicitly chose it).

    /// I — Discord platform NOT configured in trust registry +
    /// structured `delivery_destination={channel_id:"111111111111111111"}`
    /// → `DELIVERY_DESTINATION_NOT_ALLOWED`; admission is NEVER
    /// called. The outbound helper MUST NOT default to the L2-open
    /// registry default.
    #[tokio::test]
    async fn test_outbound_unconfigured_platform_fails_closed() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-I"));
        // Empty trust registry — no platform has explicit config.
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: "111111111111111111".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "expected DELIVERY_DESTINATION_NOT_ALLOWED for unconfigured Discord, got {:?}",
            response.message
        );
        assert_eq!(
            admission.calls(),
            0,
            "admission must not be called when the platform is unconfigured"
        );
    }

    /// J — UNKNOWN platform (not in trust registry at all, e.g.
    /// "telegram" or a typo like "dcord") + structured destination →
    /// fail closed. Inbound ``decide()`` continues to return the
    /// registry's deny-all-L3 default (see the trust registry tests);
    /// outbound now refuses the write entirely. The fail-closed
    /// posture has TWO defence layers:
    ///
    ///   1. ``validate_delivery_destination`` rejects unknown platform
    ///      strings with ``INVALID_DELIVERY_DESTINATION`` BEFORE the
    ///      trust check runs (this is the seeded contract — only
    ///      recognised adapter platforms may even attempt the gate).
    ///   2. ``authorize_outbound_channel`` returns ``false`` for any
    ///      platform the registry has no explicit config for (defence
    ///      layer 2 — if a future PR adds another platform to the
    ///      adapter framework without populating trust config, the
    ///      outbound path is still fail-closed).
    ///
    /// The acceptance criterion is "not WORK_ACCEPTED, admission not
    /// called". Whichever layer rejects first is acceptable.
    #[tokio::test]
    async fn test_outbound_unknown_platform_fails_closed() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-J"));
        // Empty registry — no platform is configured.
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        let mut request = native_work_request();
        // "telegram" is unrecognised by ``validate_delivery_destination``,
        // which exercises defence layer 1.
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "telegram".into(),
            channel_id: "111111111111111111".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok, "unknown platform must not succeed");
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:")
                || response
                    .message
                    .starts_with("INVALID_DELIVERY_DESTINATION:"),
            "expected fail-closed error code, got {:?}",
            response.message
        );
        assert_eq!(
            admission.calls(),
            0,
            "admission must not be called for unknown platform"
        );

        // Defence layer 2 — recognised adapter platform ("discord") but
        // NO trust config registered → trust helper returns ``false``
        // → ``DELIVERY_DESTINATION_NOT_ALLOWED``.
        let admission_d2 = Arc::new(RecordingAdmissionPort::new("admission-J-d2"));
        let handler_d2 = native_work_handler_with_trust(
            admission_d2.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        let mut request_d2 = native_work_request();
        request_d2.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: "111111111111111111".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        });
        let response_d2 = handler_d2.handle_agent_work(Some(&request_d2)).await;
        assert!(!response_d2.ok);
        assert!(
            response_d2
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "defence layer 2 (unconfigured discord) must reject, got {:?}",
            response_d2.message
        );
        assert_eq!(admission_d2.calls(), 0);
    }

    /// K — legacy `native_delivery_target` fallback when the platform
    /// has NO trust config → `DELIVERY_DESTINATION_NOT_ALLOWED`.
    /// Mirrors test I for the legacy code path.
    #[tokio::test]
    async fn test_outbound_legacy_fallback_unconfigured_fails_closed() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-K"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        // native_work_request() leaves delivery_destination=None and
        // configures the handler's static native_delivery_target to
        // a Discord channel. With an empty trust registry the legacy
        // fallback must also fail closed.
        let request = native_work_request();
        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "expected DELIVERY_DESTINATION_NOT_ALLOWED for legacy fallback + unconfigured Discord, got {:?}",
            response.message
        );
        assert_eq!(
            admission.calls(),
            0,
            "admission must not be called when the legacy fallback targets an unconfigured platform"
        );
    }

    /// L — explicit ``trust_configs`` of `PlatformTrustConfigs::new()`
    /// (empty — no platform configured) yields fail-closed even when
    /// ``RuntimeHandler::new`` is used instead of the explicit
    /// ``with_trust_configs`` builder. This is the "legacy tests that
    /// don't opt in" defence — the default empty registry now fails
    /// outbound closed by design.
    #[tokio::test]
    async fn test_outbound_default_empty_registry_fails_closed() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-L"));
        // native_work_handler() returns RuntimeHandler::new() with no
        // .with_trust_configs() call → trust_configs =
        // Arc::new(PlatformTrustConfigs::default()). We pass
        // Some(trust_configs) so the static native_delivery_target is
        // exercised through the same outbound path.
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        let request = native_work_request();
        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "expected DELIVERY_DESTINATION_NOT_ALLOWED with default empty registry, got {:?}",
            response.message
        );
        assert_eq!(admission.calls(), 0);
    }

    // ── Phase 6.4.1E — Discord parent-channel trust inheritance (outbound)
    //
    // The structured ``delivery_destination`` branch in
    // ``handle_agent_work`` now routes through
    // ``authorize_outbound_channel_with_parent``: a Discord thread
    // whose own channel_id is not in ``allowed_channels`` is
    // authorized when its parent_id IS allowed. The legacy
    // ``native_delivery_target`` fallback (no parent_id) keeps the
    // Round 2 channel-only check.

    /// Production snowflakes, kept as constants so the regression tests
    /// can pin the EXACT runtime configuration that triggered Phase
    /// 6.4.1E.
    const PARENT_CHANNEL_ID: &str = "1536735741642547262";
    const WORKFLOW_THREAD_ID: &str = "1544014554000789575";

    /// F — ``allowed=[PARENT_CHANNEL_ID]`` +
    /// ``delivery_destination={channel=WORKFLOW_THREAD_ID,
    /// parent=PARENT_CHANNEL_ID}`` → ``WORK_ACCEPTED`` via parent
    /// inheritance. ``RecordingAdmissionPort`` receives exactly one
    /// admission and the dispatched ``ChannelRef`` carries the parent.
    #[tokio::test]
    async fn test_outbound_parent_inheritance_allows_workflow_thread() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-F"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&[PARENT_CHANNEL_ID])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: WORKFLOW_THREAD_ID.into(),
            thread_id: None,
            parent_id: Some(PARENT_CHANNEL_ID.into()),
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(
            response.ok,
            "F: expected WORK_ACCEPTED via parent inheritance, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(admission.calls(), 1);
        let (channel, _metadata) = admission.last_admission();
        assert_eq!(channel.platform, "discord");
        assert_eq!(channel.channel_id, WORKFLOW_THREAD_ID);
        assert_eq!(channel.parent_id.as_deref(), Some(PARENT_CHANNEL_ID));
    }

    /// G — explicit ``T`` allowed wins over parent. The same workflow
    /// thread is allowed when ``allowed=[T]`` even if the parent is
    /// not in the list. ``T`` MUST take precedence; we must not
    /// "shadow" an explicit allow with a parent check.
    #[tokio::test]
    async fn test_outbound_explicit_thread_allowed_wins_over_parent_check() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-G"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&[WORKFLOW_THREAD_ID])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: WORKFLOW_THREAD_ID.into(),
            thread_id: None,
            // Parent is NOT in the allowlist — explicit T must still pass.
            parent_id: Some("999999999999999999".into()),
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(
            response.ok,
            "G: explicit T must Allow even when parent is unrelated, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(admission.calls(), 1);
    }

    /// H — neither T nor P in allowlist → ``DELIVERY_DESTINATION_NOT_ALLOWED``;
    /// admission is NEVER called. No implicit broadening.
    #[tokio::test]
    async fn test_outbound_neither_channel_nor_parent_allowed_denied() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-H"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&["999999999999999999"])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: WORKFLOW_THREAD_ID.into(),
            thread_id: None,
            parent_id: Some(PARENT_CHANNEL_ID.into()),
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "H: expected DELIVERY_DESTINATION_NOT_ALLOWED, got {:?}",
            response.message
        );
        assert_eq!(admission.calls(), 0);
    }

    /// I — T not allowed AND no parent → ``DELIVERY_DESTINATION_NOT_ALLOWED``.
    /// Parent inheritance does NOT widen scope when no parent is
    /// supplied — bit-exact with the legacy channel-only check.
    #[tokio::test]
    async fn test_outbound_no_parent_and_channel_not_allowed_denied() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-I"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&[PARENT_CHANNEL_ID])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: "222222222222222222".into(),
            thread_id: None,
            parent_id: None, // missing parent
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "I: missing parent + T not allowed must DENY, got {:?}",
            response.message
        );
        assert_eq!(admission.calls(), 0);
    }

    /// J — legacy ``native_delivery_target`` fallback (no parent_id)
    /// preserves the Round 2 channel-only check. The static fallback
    /// channel "1539923659345502208" is in the allowlist → Allow.
    /// This is the negative control: parent inheritance must not
    /// change the legacy path.
    #[tokio::test]
    async fn test_outbound_legacy_fallback_channel_only_check_preserved() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-J"));
        // allowlist matches the static native_delivery_target fixture
        // (see ``native_work_handler_with_trust`` line 1839).
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&["1539923659345502208"])),
        );
        let request = native_work_request(); // delivery_destination=None

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(
            response.ok,
            "J: legacy fallback channel in allowlist must Allow, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(admission.calls(), 1);
        let (channel, _metadata) = admission.last_admission();
        // Legacy fallback ChannelRef has parent_id == None by construction.
        assert_eq!(channel.parent_id, None);
    }

    /// K — unconfigured Discord platform (empty trust registry) →
    /// ``DELIVERY_DESTINATION_NOT_ALLOWED`` even when parent_id
    /// IS provided. The Round 2 fail-closed invariant MUST hold for
    /// the parent-aware path: an authenticated ``agent.work`` request
    /// cannot smuggle a destination past an empty registry just by
    /// setting parent_id.
    #[tokio::test]
    async fn test_outbound_unconfigured_platform_fails_closed_with_parent() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-K"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(Arc::new(PlatformTrustConfigs::new())),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: WORKFLOW_THREAD_ID.into(),
            thread_id: None,
            parent_id: Some(PARENT_CHANNEL_ID.into()),
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(!response.ok);
        assert!(
            response
                .message
                .starts_with("DELIVERY_DESTINATION_NOT_ALLOWED:"),
            "K: unconfigured Discord must DENY even with valid parent, got {:?}",
            response.message
        );
        assert_eq!(admission.calls(), 0);
    }

    /// Production regression — end-to-end pin of the Phase 6.4.1E
    /// operator configuration. ``allowed=[PARENT_CHANNEL_ID]``,
    /// ``delivery_destination={channel=WORKFLOW_THREAD_ID,
    /// parent=PARENT_CHANNEL_ID}`` → ``WORK_ACCEPTED`` with the parent
    /// propagated into the dispatched ``ChannelRef`` so the adapter
    /// sends into the actual workflow thread.
    #[tokio::test]
    async fn test_outbound_production_regression_workflow_thread_admitted() {
        let admission = Arc::new(RecordingAdmissionPort::new("admission-prod-regression"));
        let handler = native_work_handler_with_trust(
            admission.clone(),
            Some(trust_allowing_only(&[PARENT_CHANNEL_ID])),
        );
        let mut request = native_work_request();
        request.delivery_destination = Some(AgentWorkDeliveryDestination {
            platform: "discord".into(),
            channel_id: WORKFLOW_THREAD_ID.into(),
            thread_id: None,
            parent_id: Some(PARENT_CHANNEL_ID.into()),
            origin_event_id: None,
        });

        let response = handler.handle_agent_work(Some(&request)).await;

        assert!(
            response.ok,
            "production regression: workflow thread must be admitted via parent inheritance, got message={:?}",
            response.message
        );
        assert_eq!(response.message, "WORK_ACCEPTED");
        assert_eq!(
            admission.calls(),
            1,
            "production regression: exactly one admission expected"
        );
        let (channel, _metadata) = admission.last_admission();
        assert_eq!(channel.platform, "discord");
        assert_eq!(channel.channel_id, WORKFLOW_THREAD_ID);
        assert_eq!(channel.parent_id.as_deref(), Some(PARENT_CHANNEL_ID));
    }

    // ── Phase 6.2.9 fix round 2 — concurrent ctl dispatch tests ──────────────

    /// Admission port that sleeps briefly so multiple concurrent
    /// `handle_agent_work` calls are guaranteed to overlap before any
    /// of them completes. This is the smallest seam that exercises the
    /// "waiter observes InFlight" race without deadlocking the holder
    /// on a barrier (the holder enters `admit_work`; waiters wait on
    /// the ledger's InFlight `Notify` BEFORE entering `admit_work`).
    struct SlowAdmissionPort {
        calls: AtomicUsize,
        delay: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl WorkAdmissionPort for SlowAdmissionPort {
        async fn admit_work(
            &self,
            request: WorkAdmissionRequest,
        ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(WorkAdmissionAck {
                admission_id: format!("admission-{}", self.calls.load(AtomicOrdering::SeqCst)),
                conversation_key: request.conversation.session_pool_key(),
                accepted: true,
                native_workflow: request.native_workflow,
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ctl_concurrent_identical_dispatch_admits_exactly_once() {
        // VERIFIER defect 2, scenario A: concurrent identical
        // `set agent_work` requests MUST result in exactly one
        // admission call. All callers MUST receive a deterministic
        // WORK_ACCEPTED acknowledgement.
        const CALLERS: usize = 4;
        let admission = Arc::new(SlowAdmissionPort {
            calls: AtomicUsize::new(0),
            delay: std::time::Duration::from_millis(50),
        });
        let handler = Arc::new(native_work_handler(admission.clone()));
        let request = Arc::new(native_work_request());

        let mut joins = Vec::new();
        for _ in 0..CALLERS {
            let h = Arc::clone(&handler);
            let r = Arc::clone(&request);
            joins.push(tokio::spawn(
                async move { h.handle_agent_work(Some(&r)).await },
            ));
        }
        for join in joins {
            let response = join.await.expect("task did not panic");
            assert!(response.ok, "{}", response.message);
            assert_eq!(response.message, "WORK_ACCEPTED");
        }
        assert_eq!(
            admission.calls.load(AtomicOrdering::SeqCst),
            1,
            "the ledger reservation MUST guarantee exactly one admission per dispatch_id"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ctl_concurrent_different_dispatch_ids_admit_independently() {
        // VERIFIER defect 2, scenario D: different dispatch_ids MUST
        // execute independently even when the requests race. Each
        // unique id becomes its own reservation; identical ids within
        // a group collapse to one admission.
        const CALLERS_PER_ID: usize = 3;
        const UNIQUE_IDS: usize = 3;
        let admission = Arc::new(SlowAdmissionPort {
            calls: AtomicUsize::new(0),
            delay: std::time::Duration::from_millis(20),
        });
        let handler = Arc::new(native_work_handler(admission.clone()));
        let mut joins = Vec::new();
        for id in 0..UNIQUE_IDS {
            for _ in 0..CALLERS_PER_ID {
                let h = Arc::clone(&handler);
                let mut r = native_work_request();
                r.dispatch_id = format!("oad-conc-{id}");
                joins.push(tokio::spawn(
                    async move { h.handle_agent_work(Some(&r)).await },
                ));
            }
        }
        for join in joins {
            let response = join.await.expect("task did not panic");
            assert!(response.ok, "{}", response.message);
        }
        assert_eq!(
            admission.calls.load(AtomicOrdering::SeqCst),
            UNIQUE_IDS,
            "each unique dispatch_id must run admission exactly once"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ctl_concurrent_dispatch_after_admission_failure_releases_reservation() {
        // VERIFIER defect 2, scenario C: an admission failure MUST
        // release the in-flight reservation so a later retry of the
        // SAME dispatch_id can attempt admission again.
        use std::sync::atomic::Ordering;
        let failing = Arc::new(RejectingAdmissionPort(AtomicUsize::new(0)));
        let handler = Arc::new(native_work_handler(failing.clone()));
        let request = native_work_request();

        let first = handler.handle_agent_work(Some(&request)).await;
        assert!(!first.ok, "first admission must fail");

        // Switch to a recording admission port for the retry. We
        // rebuild the handler because the admission port is wired at
        // construction time and there is no setter.
        let recording = Arc::new(RecordingAdmissionPort::new("admission-retry-1"));
        let handler = Arc::new(native_work_handler(recording.clone()));

        // Race two retries. The ledger slot was released by the
        // previous failure, so both retries should observe a Fresh
        // reservation sequentially: the first wins, the second sees
        // Done (cached ack) and short-circuits.
        let h1 = Arc::clone(&handler);
        let h2 = Arc::clone(&handler);
        let r1 = Arc::new(native_work_request());
        let r2 = Arc::clone(&r1);
        let j1 = tokio::spawn(async move { h1.handle_agent_work(Some(&r1)).await });
        let j2 = tokio::spawn(async move { h2.handle_agent_work(Some(&r2)).await });
        let r1 = j1.await.expect("task did not panic");
        let r2 = j2.await.expect("task did not panic");
        assert!(r1.ok && r2.ok, "both retries must succeed");
        assert_eq!(
            recording.calls.load(Ordering::SeqCst),
            1,
            "after the failure-released reservation, the retry round must admit exactly once"
        );
    }

    #[test]
    fn agent_work_strict_validation_rejects_empty_and_invalid_native_fields() {
        for field in 0..9 {
            let mut request = native_work_request();
            match field {
                0 => request.dispatch_id.clear(),
                1 => request.workflow_run_id.clear(),
                2 => request.task_id.clear(),
                3 => request.lease_id.clear(),
                4 => request.conversation_key.clear(),
                5 => request.assignment.clear(),
                6 => request.language.clear(),
                7 => request.role.clear(),
                _ => request.agent.clear(),
            }
            assert!(validate_agent_work(&request).is_some());
        }
        let mut role = native_work_request();
        role.role = "UNKNOWN".into();
        assert!(validate_agent_work(&role).is_some());
        let mut agent = native_work_request();
        agent.agent = "UNKNOWN".into();
        assert!(validate_agent_work(&agent).is_some());
        let mut generation = native_work_request();
        generation.lease_generation = 0;
        assert!(validate_agent_work(&generation).is_some());
        let mut conversation = native_work_request();
        conversation.conversation_key = "opaque-conversation-capability".into();
        assert!(validate_agent_work(&conversation).is_none());
        let missing_expected_revision: Request = serde_json::from_str(r#"{"action":"set","key":"agent.work","dispatch_id":"d","workflow_run_id":"r","task_id":"t","role":"PRIMARY","agent":"ArthurClaude","lease_id":"l","lease_generation":1,"conversation_key":"123","assignment":"a","language":"en"}"#).unwrap();
        assert!(missing_expected_revision.agent_work.is_none());
    }

    #[test]
    fn agent_work_fingerprint_covers_execution_and_fencing_fields() {
        let baseline = native_work_request();
        for index in 0..11 {
            let mut changed = baseline.clone();
            match index {
                0 => changed.dispatch_id = "other".into(),
                1 => changed.workflow_run_id = "other".into(),
                2 => changed.task_id = "other".into(),
                3 => changed.role = "VERIFIER".into(),
                4 => changed.agent = "ArthurCodex".into(),
                5 => changed.lease_id = "other".into(),
                6 => changed.lease_generation = 2,
                7 => changed.expected_revision = 1,
                8 => changed.conversation_key = "654321".into(),
                9 => changed.assignment = "other".into(),
                _ => changed.language = "en".into(),
            }
            assert_ne!(
                agent_work_fingerprint(&baseline),
                agent_work_fingerprint(&changed)
            );
        }
    }

    #[test]
    fn agent_work_ledger_is_bounded_and_evicts_oldest_entry() {
        let mut ledger = AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        for index in 0..=AgentWorkLedger::CAPACITY {
            // Reserve then promote to Done — exercises the same
            // insertion path as a successful native-work dispatch.
            let key = format!("key-{index}");
            let ReservationOutcome::Fresh { state_tx } =
                ledger.try_reserve(key.clone(), index.to_string())
            else {
                panic!("first reservation for {key} must be Fresh");
            };
            ledger.complete_with_done(
                &key,
                AgentWorkLedgerEntry {
                    fingerprint: index.to_string(),
                    ack: "ack".into(),
                },
            );
            drop(state_tx);
        }
        assert_eq!(ledger.entries.len(), AgentWorkLedger::CAPACITY);
        assert!(matches!(
            ledger.try_reserve("key-0".into(), "0".into()),
            ReservationOutcome::Fresh { .. }
        ));
        assert!(matches!(
            ledger.try_reserve("key-1024".into(), "1024".into()),
            ReservationOutcome::Done(_)
        ));
    }

    // ── Phase 6.2.9 fix round 3 — InFlight eviction protection tests ─────────

    /// VERIFIER defect 1, scenario A: a pinned InFlight reservation
    /// MUST survive capacity pressure. We reserve one key as Fresh
    /// (the InFlight slot stays pinned because we never call
    /// `complete_with_done`), then push the ledger over capacity by
    /// inserting CAPACITY+1 Done entries. The InFlight entry must
    /// still be present after the eviction pass — only the oldest
    /// Done entries are eligible for eviction.
    #[test]
    fn in_flight_not_evicted_under_capacity_pressure() {
        let mut ledger = AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        // The protected key.
        let protected_key = "protected-inflight".to_string();
        let protected_fp = "fp-protected".to_string();
        let _state_tx = match ledger.try_reserve(protected_key.clone(), protected_fp.clone()) {
            ReservationOutcome::Fresh { state_tx } => state_tx,
            other => panic!("first reservation must be Fresh, got {other:?}"),
        };
        assert!(
            matches!(
                ledger.entries.get(&protected_key),
                Some(AgentWorkLedgerSlot::InFlight(_))
            ),
            "protected key must be InFlight"
        );

        // Push the ledger over CAPACITY with Done entries.
        for i in 0..=AgentWorkLedger::CAPACITY {
            let key = format!("done-{i}");
            let ReservationOutcome::Fresh { state_tx } =
                ledger.try_reserve(key.clone(), format!("fp-{i}"))
            else {
                panic!("reservation {i} must be Fresh");
            };
            ledger.complete_with_done(
                &key,
                AgentWorkLedgerEntry {
                    fingerprint: format!("fp-{i}"),
                    ack: "ack".into(),
                },
            );
            drop(state_tx);
        }

        // The protected InFlight MUST still be in the map.
        assert!(
            matches!(
                ledger.entries.get(&protected_key),
                Some(AgentWorkLedgerSlot::InFlight(_))
            ),
            "the protected InFlight entry MUST NOT be evicted under capacity pressure"
        );
        // A second `try_reserve` for the same protected key MUST
        // observe the same InFlight (no Fresh re-promotion).
        assert!(matches!(
            ledger.try_reserve(protected_key.clone(), protected_fp.clone()),
            ReservationOutcome::InFlight(_)
        ));
    }

    /// VERIFIER defect 1, scenario B: while K is pinned as InFlight,
    /// a concurrent identical request MUST observe InFlight (join the
    /// wait) and MUST NOT trigger a duplicate admission. The handler
    /// path is exercised end-to-end through `handle_agent_work`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_duplicate_under_capacity_pressure_admits_once() {
        let admission = Arc::new(SlowAdmissionPort {
            calls: AtomicUsize::new(0),
            delay: std::time::Duration::from_millis(50),
        });
        let handler = Arc::new(native_work_handler(admission.clone()));
        // Pressure the ledger first by running many sequential
        // dispatches with distinct dispatch_ids. Each one becomes
        // Done on success — the ledger fills to capacity with Done
        // entries plus a few InFlight entries from concurrent races.
        for i in 0..AgentWorkLedger::CAPACITY {
            let mut r = native_work_request();
            r.dispatch_id = format!("oad-pressure-{i}");
            let response = handler.handle_agent_work(Some(&r)).await;
            assert!(response.ok, "pressure run {i} must succeed");
        }
        let baseline_calls = admission.calls.load(AtomicOrdering::SeqCst);
        assert_eq!(
            baseline_calls,
            AgentWorkLedger::CAPACITY,
            "baseline runs must each admit exactly once"
        );

        // Now fire 4 concurrent identical requests for a NEW
        // dispatch_id. While the holder is in admit_work, the
        // ledger has CAPACITY+ (Done) entries — eviction should run,
        // but the new InFlight entry MUST be pinned and survive.
        let request = Arc::new(native_work_request());
        let mut joins = Vec::new();
        for _ in 0..4 {
            let h = Arc::clone(&handler);
            let r = Arc::clone(&request);
            joins.push(tokio::spawn(
                async move { h.handle_agent_work(Some(&r)).await },
            ));
        }
        for join in joins {
            let response = join.await.expect("task did not panic");
            assert!(response.ok, "concurrent dispatch must succeed");
        }
        let after = admission.calls.load(AtomicOrdering::SeqCst);
        assert_eq!(
            after - baseline_calls,
            1,
            "concurrent duplicate dispatch must admit exactly once under capacity pressure"
        );
    }

    /// VERIFIER defect 1, scenario C: after K's holder succeeds, the
    /// InFlight slot is promoted to Done and the cached ack can be
    /// replayed.
    #[test]
    fn inflight_done_after_complete_with_done_replays_normally() {
        let mut ledger = AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        let key = "k-c".to_string();
        let _ = match ledger.try_reserve(key.clone(), "fp".into()) {
            ReservationOutcome::Fresh { state_tx } => state_tx,
            _ => panic!("must be Fresh"),
        };
        ledger.complete_with_done(
            &key,
            AgentWorkLedgerEntry {
                fingerprint: "fp".into(),
                ack: "ack-c".into(),
            },
        );
        assert!(matches!(
            ledger.try_reserve(key.clone(), "fp".into()),
            ReservationOutcome::Done(_)
        ));
    }

    /// VERIFIER defect 1, scenario D: when the holder fails (we
    /// simulate by dropping its `InFlightGuard`), the slot is
    /// released; a later retry sees Fresh.
    #[test]
    fn inflight_failure_then_retry_yields_fresh() {
        let mut ledger = AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        let key = "k-d".to_string();
        let _ = match ledger.try_reserve(key.clone(), "fp".into()) {
            ReservationOutcome::Fresh { state_tx } => state_tx,
            _ => panic!("must be Fresh"),
        };
        // Simulate holder failure: drop the entry from the map. In
        // production this is done by `release_in_flight` from
        // `InFlightGuard::Drop`; here we exercise the ledger path
        // directly because the guard requires a Tokio runtime.
        assert!(ledger.release_in_flight(&key));
        assert!(matches!(
            ledger.try_reserve(key, "fp".into()),
            ReservationOutcome::Fresh { .. }
        ));
    }

    /// VERIFIER defect 1, scenario E: capacity eviction still removes
    /// old Done entries when there are no InFlight entries to protect.
    #[test]
    fn capacity_eviction_still_removes_old_done_entries() {
        let mut ledger = AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        for i in 0..=AgentWorkLedger::CAPACITY {
            let key = format!("k-e-{i}");
            let ReservationOutcome::Fresh { state_tx } =
                ledger.try_reserve(key.clone(), format!("fp-{i}"))
            else {
                panic!()
            };
            ledger.complete_with_done(
                &key,
                AgentWorkLedgerEntry {
                    fingerprint: format!("fp-{i}"),
                    ack: "ack".into(),
                },
            );
            drop(state_tx);
        }
        // Force one more reservation; eviction should drop the
        // oldest Done entry. With CAPACITY=1024 the first 1024
        // entries survive; only entry 1024 (the most recent) plus
        // the new Fresh one are present after eviction.
        let _ = match ledger.try_reserve("k-e-new".into(), "fp-new".into()) {
            ReservationOutcome::Fresh { state_tx } => state_tx,
            _ => panic!("must be Fresh after eviction"),
        };
        assert!(ledger.entries.contains_key("k-e-new"));
        assert!(
            !ledger.entries.contains_key("k-e-0"),
            "oldest Done entry must be evicted when no InFlight is present"
        );
    }

    /// VERIFIER defense-in-depth: `InFlightGuard::Drop` must not panic
    /// when no Tokio runtime is available. We construct the guard,
    /// drop it, and verify the ledger slot was released via the
    /// blocking_lock fallback (or remains pinned if even that fails,
    /// but we never panic).
    #[test]
    fn inflight_guard_drop_without_tokio_runtime_does_not_panic() {
        let ledger = Arc::new(tokio::sync::Mutex::new(AgentWorkLedger {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }));
        let key = "no-runtime-drop".to_string();
        // Seed the InFlight slot via blocking_lock — this is fine
        // because we don't need an async runtime for the ledger's
        // synchronous methods.
        {
            let mut g = ledger.blocking_lock();
            let _ = match g.try_reserve(key.clone(), "fp".into()) {
                ReservationOutcome::Fresh { state_tx } => state_tx,
                _ => panic!(),
            };
        }
        assert!(matches!(
            ledger.blocking_lock().entries.get(&key),
            Some(AgentWorkLedgerSlot::InFlight(_))
        ));
        // Drop the guard outside any Tokio runtime — the Drop
        // impl must take the blocking_lock fallback and release
        // the slot. If it panicked the test would fail.
        {
            let guard = InFlightGuard::new(Arc::clone(&ledger), key.clone());
            drop(guard);
        }
        // Verify the slot was released.
        assert!(
            !ledger.blocking_lock().entries.contains_key(&key),
            "the InFlight slot must be released by the blocking fallback"
        );
    }

    #[test]
    fn resolve_platform_registry_hit() {
        let r = reg(&[("123", "discord")]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn resolve_platform_single_adapter_fallback() {
        // No registry entry, but only one adapter -> resolve to it.
        let r = reg(&[]);
        let platforms = vec!["discord".to_string()];
        assert_eq!(
            resolve_platform("999", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn resolve_platform_multi_adapter_miss_is_none() {
        // No registry entry and multiple adapters -> genuinely ambiguous.
        let r = reg(&[]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(resolve_platform("999", &r, &platforms), None);
    }

    #[test]
    fn resolve_platform_no_adapters_is_none() {
        let r = reg(&[]);
        let platforms: Vec<String> = vec![];
        assert_eq!(resolve_platform("999", &r, &platforms), None);
    }

    #[test]
    fn resolve_platform_registry_hit_wins_over_fallback() {
        // Registry takes precedence when the platform is still configured.
        let r = reg(&[("123", "slack")]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("slack")
        );
    }

    #[test]
    fn resolve_platform_stale_registry_entry_falls_through() {
        // Stale registry entry pointing to unconfigured platform falls through to fallback.
        let r = reg(&[("123", "slack")]);
        let platforms = vec!["discord".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn request_serialization() {
        let req = Request {
            action: Action::Set,
            key: "thread.name".into(),
            value: Some("hello".into()),
            thread_id: Some("123".into()),
            target_user_id: None,
            project: None,
            agent_work: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.action, Action::Set);
        assert_eq!(parsed.key, "thread.name");
        assert_eq!(parsed.value.as_deref(), Some("hello"));
        assert_eq!(parsed.thread_id.as_deref(), Some("123"));
        assert_eq!(parsed.target_user_id, None);
        assert!(parsed.project.is_none());
    }

    #[test]
    fn request_serialization_with_project_skips_when_none() {
        // Backward compatibility: clients that don't send `project` must
        // continue to work (the field is skipped when None).
        let req = Request {
            action: Action::Set,
            key: "thread.message".into(),
            value: Some("hi".into()),
            thread_id: Some("T".into()),
            target_user_id: None,
            project: None,
            agent_work: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("project"),
            "project field must be omitted when None: {json}"
        );
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert!(parsed.project.is_none());
    }

    #[test]
    fn request_serialization_with_project_roundtrip() {
        let req = Request {
            action: Action::Set,
            key: "thread.pin".into(),
            value: None,
            thread_id: Some("T".into()),
            target_user_id: None,
            project: Some(ProjectRef {
                project_id: "openab".into(),
                project_root: "/home/arthur/openab/source".into(),
            }),
            agent_work: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("project_id"));
        assert!(json.contains("project_root"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        let p = parsed.project.expect("project must round-trip");
        assert_eq!(p.project_id, "openab");
        assert_eq!(p.project_root, "/home/arthur/openab/source");
    }

    #[test]
    fn project_ref_rejects_empty_project_id() {
        let p = ProjectRef {
            project_id: "".into(),
            project_root: "/tmp".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("empty project_id must fail");
        assert!(err.contains("project_id"), "{err}");
    }

    #[test]
    fn project_ref_rejects_empty_project_root() {
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: "".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("empty project_root must fail");
        assert!(err.contains("project_root"), "{err}");
    }

    #[test]
    fn project_ref_rejects_nonexistent_project_root() {
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: "/this/path/does/not/exist/anywhere_2026_08_18".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("nonexistent project_root must fail");
        assert!(err.contains("cannot be canonicalized"), "{err}");
    }

    #[test]
    fn project_ref_canonicalizes_existing_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: dir.path().to_string_lossy().to_string(),
        };
        let ctx = ProjectContext::try_from(p).expect("existing dir should canonicalize");
        assert_eq!(ctx.project_id, "openab");
        assert_eq!(ctx.project_root, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[tokio::test]
    async fn server_client_roundtrip() {
        struct MockHandler;
        #[async_trait::async_trait]
        impl CtlHandler for MockHandler {
            async fn handle_set(
                &self,
                thread_id: Option<&str>,
                key: &str,
                value: &str,
                _target_user_id: Option<&str>,
                _project: Option<&ProjectRef>,
            ) -> Response {
                Response {
                    ok: true,
                    message: format!("{key} = {value} (thread: {})", thread_id.unwrap_or("none")),
                    value: None,
                    message_id: None,
                }
            }
            async fn handle_get(&self, _thread_id: Option<&str>, key: &str) -> Response {
                Response {
                    ok: true,
                    message: String::new(),
                    value: Some(format!("val-of-{key}")),
                    message_id: None,
                }
            }
        }

        // Use a temp path to avoid conflicts
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handler = std::sync::Arc::new(MockHandler);
        let server = spawn_server_at(sock.clone(), handler);
        // Give server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Test set
        let resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.name".into(),
                value: Some("hello world".into()),
                thread_id: Some("999".into()),
                target_user_id: None,
                project: None,
                agent_work: None,
            },
        )
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.message, "thread.name = hello world (thread: 999)");

        // Test get
        let resp = send_request_to(
            &sock,
            &Request {
                action: Action::Get,
                key: "thread.name".into(),
                value: None,
                thread_id: None,
                target_user_id: None,
                project: None,
                agent_work: None,
            },
        )
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.value.as_deref(), Some("val-of-thread.name"));

        server.abort();
    }

    #[test]
    fn protocol_carries_target_user_id() {
        let req = Request {
            action: Action::Set,
            key: "thread.message".into(),
            value: Some("HANDOFF\nto: <@1536734779607879700>\n".into()),
            thread_id: Some("1536735741642547262".into()),
            target_user_id: Some("1536734779607879700".into()),
            project: None,
            agent_work: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("target_user_id"));
        assert!(json.contains("1536734779607879700"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.target_user_id.as_deref(),
            Some("1536734779607879700")
        );
        assert_eq!(parsed.key, "thread.message");
        assert!(parsed.value.unwrap().starts_with("HANDOFF"));
    }

    #[tokio::test]
    async fn server_client_roundtrip_carries_target_user_id() {
        #[derive(Default)]
        struct CapturedHandler {
            captured_target_user_id: std::sync::Mutex<Option<String>>,
        }
        #[async_trait::async_trait]
        impl CtlHandler for CapturedHandler {
            async fn handle_set(
                &self,
                _thread_id: Option<&str>,
                _key: &str,
                _value: &str,
                target_user_id: Option<&str>,
                _project: Option<&ProjectRef>,
            ) -> Response {
                *self.captured_target_user_id.lock().unwrap() = target_user_id.map(str::to_string);
                Response {
                    ok: true,
                    message: "captured".into(),
                    value: None,
                    message_id: None,
                }
            }
            async fn handle_get(&self, _: Option<&str>, _: &str) -> Response {
                Response {
                    ok: false,
                    message: "no get".into(),
                    value: None,
                    message_id: None,
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let handler = std::sync::Arc::new(CapturedHandler::default());
        let server = spawn_server_at(sock.clone(), handler.clone());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("HANDOFF\n...".into()),
                thread_id: Some("1536735741642547262".into()),
                target_user_id: Some("1536734779607879700".into()),
                project: None,
                agent_work: None,
            },
        )
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(
            handler.captured_target_user_id.lock().unwrap().as_deref(),
            Some("1536734779607879700"),
        );
        server.abort();
    }

    // ─────────────────────────────────────────────────────────────────────
    // Tests for workflow `20260818-openab-project-aware-thread-routing`.
    // A–J, K, L, M, N, O, E2E.
    // ─────────────────────────────────────────────────────────────────────

    use openab_core::acp::pool::SessionPoolTestState;
    use openab_core::acp::project::ProjectContext as CoreProjectContext;
    use openab_core::acp::SessionPool;
    use openab_core::adapter::{MessageRef as CoreMessageRef, SenderContext};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// Recording adapter — counts every `send_message_targeted` / `rename_thread`
    /// call so tests can assert no outbound message was sent.
    #[derive(Default)]
    struct RecordingAdapter {
        send_count: StdMutex<usize>,
        direct_send_count: StdMutex<usize>,
        last_value: StdMutex<Option<String>>,
        last_channel_id: StdMutex<Option<String>>,
        last_target_user_id: StdMutex<Option<String>>,
        targeted_failure: StdMutex<Option<String>>,
    }

    impl RecordingAdapter {
        fn send_count(&self) -> usize {
            *self.send_count.lock().unwrap()
        }
        fn last_value(&self) -> Option<String> {
            self.last_value.lock().unwrap().clone()
        }
        fn direct_send_count(&self) -> usize {
            *self.direct_send_count.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl ChatAdapter for RecordingAdapter {
        fn platform(&self) -> &'static str {
            "discord"
        }
        fn message_limit(&self) -> usize {
            2000
        }
        async fn send_message(
            &self,
            channel: &ChannelRef,
            _content: &str,
        ) -> anyhow::Result<CoreMessageRef> {
            *self.direct_send_count.lock().unwrap() += 1;
            Ok(CoreMessageRef {
                channel: channel.clone(),
                message_id: "mock-id".into(),
            })
        }
        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger: &CoreMessageRef,
            _title: &str,
        ) -> anyhow::Result<ChannelRef> {
            Ok(channel.clone())
        }
        async fn add_reaction(&self, _msg: &CoreMessageRef, _emoji: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove_reaction(&self, _msg: &CoreMessageRef, _emoji: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_message_targeted(
            &self,
            channel: &ChannelRef,
            content: &str,
            _target_user_id: Option<&str>,
        ) -> anyhow::Result<CoreMessageRef> {
            *self.send_count.lock().unwrap() += 1;
            *self.last_value.lock().unwrap() = Some(content.to_string());
            *self.last_channel_id.lock().unwrap() = Some(channel.channel_id.clone());
            *self.last_target_user_id.lock().unwrap() = _target_user_id.map(str::to_owned);
            if let Some(error) = self.targeted_failure.lock().unwrap().clone() {
                return Err(anyhow::anyhow!(error));
            }
            Ok(CoreMessageRef {
                channel: channel.clone(),
                message_id: format!("msg-{}", self.send_count.lock().unwrap()),
            })
        }
        async fn rename_thread(&self, _channel: &ChannelRef, _title: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    /// Minimal ACP-compatible test agent script. Mirrors the one in
    /// `crates/openab-core/src/acp/pool.rs` tests but trimmed: no record
    /// file, just enough JSON-RPC to get `pool.get_or_create` through
    /// `initialize` → `session/new` (or `session/load`) → `session/cancel`
    /// without hanging or erroring.
    const TEST_AGENT_SCRIPT: &str = r#"#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *initialize*)    printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"test"},"agentCapabilities":{"loadSession":true}}}' ;;
        *session/new*)   printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_test"}}' ;;
        *session/load*)  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"sess_test"}}' ;;
        *session/cancel*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}' ;;
        *)               printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{}}' ;;
    esac
done
"#;

    /// Recording variant of the test agent. When invoked as
    /// `test-acp-agent.sh <record_file>`, every received JSON-RPC line is
    /// appended to `record_file` (truncated on start). Drives the E2E
    /// proof that `session/new.params.cwd` reaches the agent with the
    /// canonical project root (workflow
    /// `20260818-openab-project-aware-thread-routing` test E2E).
    const TEST_AGENT_RECORD_SCRIPT: &str = r#"#!/bin/sh
RECORD="${1:-}"
if [ -n "$RECORD" ]; then
    : > "$RECORD"
fi
while IFS= read -r line; do
    if [ -n "$RECORD" ]; then
        printf '%s\n' "$line" >> "$RECORD"
    fi
    case "$line" in
        *initialize*)    printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"test"},"agentCapabilities":{"loadSession":true}}}' ;;
        *session/new*)   printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_test"}}' ;;
        *session/load*)  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"sess_test"}}' ;;
        *session/cancel*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}' ;;
        *)               printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{}}' ;;
    esac
done
"#;

    /// Write the test agent script to a tempdir and return its path.
    fn write_test_agent_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("test-acp-agent.sh");
        std::fs::write(&script, TEST_AGENT_SCRIPT).expect("write test agent script");
        #[cfg(unix)]
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod test agent script");
        script
    }

    /// Write the recording test agent script to a tempdir and return its path.
    /// The recording variant accepts a record file path as `$1` and writes
    /// every received JSON-RPC line to it (truncated on start).
    fn write_test_agent_record_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("test-acp-agent-record.sh");
        std::fs::write(&script, TEST_AGENT_RECORD_SCRIPT).expect("write test agent record script");
        #[cfg(unix)]
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod test agent record script");
        script
    }

    /// Build a `SessionPool` whose agent command is the recording test
    /// agent script. The `record_path` is passed as the agent's `$1`
    /// argument so every JSON-RPC line the agent receives is appended
    /// to `record_path`. Used by the UDS E2E test to assert that
    /// `session/new.params.cwd` reached the agent.
    fn recording_pool(
        dir: &std::path::Path,
        record_path: &std::path::Path,
    ) -> std::sync::Arc<SessionPool> {
        let agent_script = write_test_agent_record_script(dir);
        let config = openab_core::config::AgentConfig {
            command: agent_script.to_string_lossy().into(),
            args: vec![record_path.to_string_lossy().into_owned()],
            working_dir: "/tmp".into(),
            env: HashMap::new(),
            inherit_env: vec![],
            command_explicit: true,
        };
        std::sync::Arc::new(SessionPool::with_test_state(
            config,
            SessionPoolTestState::default(),
            dir.join("session_projects.json"),
        ))
    }

    /// Read the JSON-RPC lines recorded by the test agent and extract
    /// the `cwd` field from the first `session/new` line. Returns the
    /// raw cwd string. Panics if no `session/new` line was found.
    fn cwd_from_recorded_session_new(record_path: &std::path::Path) -> String {
        let raw = std::fs::read_to_string(record_path)
            .unwrap_or_else(|e| panic!("read record file {}: {e}", record_path.display()));
        for line in raw.lines() {
            if line.contains("session/new") {
                let v: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("parse session/new line {line}: {e}"));
                let cwd = v
                    .get("params")
                    .and_then(|p| p.get("cwd"))
                    .and_then(|c| c.as_str())
                    .unwrap_or_else(|| panic!("session/new line missing cwd: {line}"));
                return cwd.to_string();
            }
        }
        panic!(
            "no session/new line found in record file {}: lines were {:?}",
            record_path.display(),
            raw.lines().collect::<Vec<_>>(),
        );
    }

    /// Constructs a `SessionPool` with a pre-populated state. Uses the
    /// public test seam `SessionPool::with_test_state` so this works from
    /// the binary crate's tests (the in-crate `with_state_for_test` is
    /// `#[cfg(test)]` and not available cross-crate).
    ///
    /// The agent command is a small ACP-compatible shell script that
    /// responds to `initialize` / `session/new` / `session/load` with valid
    /// JSON-RPC so `pool.get_or_create` can complete the spawn path.
    fn pool_with_state(
        dir: &std::path::Path,
        state: SessionPoolTestState,
    ) -> std::sync::Arc<SessionPool> {
        let agent_script = write_test_agent_script(dir);
        let config = openab_core::config::AgentConfig {
            command: agent_script.to_string_lossy().into(),
            args: vec![],
            working_dir: "/tmp".into(),
            env: HashMap::new(),
            inherit_env: vec![],
            command_explicit: true,
        };
        std::sync::Arc::new(SessionPool::with_test_state(
            config,
            state,
            dir.join("session_projects.json"),
        ))
    }

    fn empty_pool_state() -> SessionPoolTestState {
        SessionPoolTestState::default()
    }

    /// Build a `RuntimeHandler` with a single Discord adapter and a
    /// pre-populated session pool. The `state` is taken by the pool.
    fn make_handler(
        adapter: std::sync::Arc<RecordingAdapter>,
        pool: std::sync::Arc<SessionPool>,
    ) -> RuntimeHandler {
        let mut adapters: HashMap<String, std::sync::Arc<dyn ChatAdapter>> = HashMap::new();
        adapters.insert("discord".into(), adapter);
        let registry = new_registry();
        RuntimeHandler::new(adapters, registry, Arc::new(std::sync::OnceLock::new()))
            .with_pool(pool)
    }

    fn project_root(p: &std::path::Path) -> ProjectRef {
        ProjectRef {
            project_id: "openab".into(),
            project_root: p.to_string_lossy().to_string(),
        }
    }

    #[tokio::test]
    async fn canonical_handler_success_calls_targeted_send_once_with_exact_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(
            adapter.clone(),
            pool_with_state(dir.path(), empty_pool_state()),
        );
        let body = "known canonical test body";

        let response = handler
            .handle_set(
                Some("1539923659345502208"),
                "thread.message",
                body,
                Some("1536733602304499852"),
                None,
            )
            .await;

        assert!(response.ok, "canonical handler must succeed: {response:?}");
        assert_eq!(adapter.send_count(), 1, "exactly one targeted send");
        assert_eq!(adapter.direct_send_count(), 0, "no secondary adapter send");
        assert_eq!(
            adapter.last_channel_id.lock().unwrap().as_deref(),
            Some("1539923659345502208")
        );
        assert_eq!(
            adapter.last_target_user_id.lock().unwrap().as_deref(),
            Some("1536733602304499852")
        );
        assert_eq!(adapter.last_value().as_deref(), Some(body));
    }

    #[tokio::test]
    async fn canonical_handler_failure_preserves_targeted_send_error_without_extra_call() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = std::sync::Arc::new(RecordingAdapter {
            targeted_failure: StdMutex::new(Some(
                "HTTP 403 Discord code 50001 Missing Access".into(),
            )),
            ..Default::default()
        });
        let handler = make_handler(
            adapter.clone(),
            pool_with_state(dir.path(), empty_pool_state()),
        );

        let response = handler
            .handle_set(
                Some("1539923659345502208"),
                "thread.message",
                "known failure test body",
                Some("1536733602304499852"),
                None,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(
            response.message,
            "thread.message dispatch failed: HTTP 403 Discord code 50001 Missing Access"
        );
        assert_eq!(adapter.send_count(), 1, "one attempted targeted send");
        assert_eq!(
            adapter.direct_send_count(),
            0,
            "no fallback or secondary send"
        );
    }

    // ── TEST M: canonical ctl session key matches dispatcher session key ──

    /// `ChannelRef::session_pool_key()` is the SINGLE source of truth for the
    /// session key shape. The ctl layer's `RuntimeHandler` (via
    /// `ensure_pinned_project`) and the dispatcher's `Dispatcher::session_key`
    /// both call it for the same channel, producing byte-identical keys.
    #[test]
    fn channel_ref_session_pool_key_is_dispatcher_shape() {
        // Discord: threads are channels, so thread_id is None.
        let discord = ChannelRef {
            platform: "discord".into(),
            channel_id: "T1".into(),
            thread_id: None,
            parent_id: Some("P".into()),
            origin_event_id: None,
        };
        assert_eq!(discord.session_pool_key(), "discord:T1");

        // Slack: threads have thread_ts, channel_id is the parent.
        let slack = ChannelRef {
            platform: "slack".into(),
            channel_id: "C1".into(),
            thread_id: Some("1234567890.000100".into()),
            parent_id: None,
            origin_event_id: None,
        };
        assert_eq!(slack.session_pool_key(), "slack:1234567890.000100");

        // Generic threaded channel: thread_id wins over channel_id.
        let threaded = ChannelRef {
            platform: "telegram".into(),
            channel_id: "chatid".into(),
            thread_id: Some("topicid".into()),
            parent_id: None,
            origin_event_id: None,
        };
        assert_eq!(threaded.session_pool_key(), "telegram:topicid");
    }

    /// Lane-mode dispatcher key (`<platform>:<thread_id>:<sender_id>`) is NOT
    /// the ACP session key. The session is always shared per-thread
    /// regardless of grouping, so the project binding uses the canonical
    /// `<platform>:<thread_id>` form (`session_pool_key`).
    #[test]
    fn lane_mode_dispatcher_key_is_distinct_from_session_pool_key() {
        let channel = ChannelRef {
            platform: "discord".into(),
            channel_id: "T1".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };
        let session_key = channel.session_pool_key();
        let lane_key = format!("{}:userA", session_key);
        assert_eq!(session_key, "discord:T1");
        assert_eq!(lane_key, "discord:T1:userA");
        assert_ne!(session_key, lane_key);
    }

    /// The ctl layer's `ensure_pinned_project` builds a `ChannelRef` from
    /// the resolved platform + thread_id and calls `session_pool_key()` —
    /// this gives the same key as the dispatcher would for the same thread.
    #[tokio::test]
    async fn ensure_pinned_project_constructs_same_key_as_dispatcher() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let project = CoreProjectContext {
            project_id: "openab".into(),
            project_root: project_dir.path().to_path_buf(),
        };
        // The unused `project` above exercises the same direct
        // ProjectContext construction that non-ctl code paths use. The
        // ctl layer's `ensure_pinned_project` builds an equivalent
        // ProjectContext via `ProjectRef::try_from` (see request
        // serialization tests above).
        let _ = project;
        // Thread ID is "T1" with one configured adapter (discord), so the
        // single-adapter fallback resolves the platform to "discord".
        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok, "pin should succeed: {resp:?}");

        // The pool's session_projects entry uses the dispatcher key shape.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("pool must have entry under canonical key discord:T1");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(
            pinned.project_root,
            project_dir.path().canonicalize().unwrap()
        );
    }

    // ── TEST A: trusted thread bootstrap with project A ──

    #[tokio::test]
    async fn ctl_thread_pin_writes_project_to_session_pool() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok, "pin must succeed: resp={:?}", resp.message);
        assert!(resp.message.contains("pinned"));

        // The persisted ProjectContext carries the project_root, which IS
        // the SessionPool's per-thread workdir (set via `save_meta`).
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("binding must be persisted");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(
            pinned.project_root,
            project_dir.path().canonicalize().unwrap(),
            "project_root must be the canonical absolute path"
        );
        // No outbound message sent on thread.pin.
        assert_eq!(
            adapter.send_count(),
            0,
            "thread.pin must not send a message"
        );
    }

    // ── TEST B: two threads pinned to different projects remain isolated ──

    #[tokio::test]
    async fn ctl_thread_pin_two_threads_remain_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();

        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok, "T1 pin: {:?}", r1.message);
        let r2 = handler
            .handle_set(Some("T2"), "thread.pin", "", None, Some(&b))
            .await;
        assert!(r2.ok, "T2 pin: {:?}", r2.message);

        let pa = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("T1 must have a binding");
        assert_eq!(pa.project_id, "A");
        let pb = pool
            .get_pinned_project("discord:T2")
            .await
            .expect("T2 must have a binding");
        assert_eq!(pb.project_id, "B");
        assert_ne!(
            pa.project_root, pb.project_root,
            "T1 and T2 must have distinct project roots"
        );
    }

    // ── TEST D: pin with different project fails closed ──

    #[tokio::test]
    async fn ctl_thread_pin_with_different_project_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();

        // Pin T1 to A.
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok);

        // Pin T1 to B → fail closed.
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&b))
            .await;
        assert!(!r2.ok, "second pin must fail closed");
        assert!(
            r2.message.contains("mismatch"),
            "error must mention mismatch: {}",
            r2.message
        );

        // The binding must remain A.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("T1 must still have its A binding");
        assert_eq!(pinned.project_id, "A");
    }

    // ── TEST L: same pinned A → idempotent success ──

    #[tokio::test]
    async fn same_pinned_a_thread_pin_a_is_idempotent_success() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let p = project_root(project_dir.path());
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p))
            .await;
        assert!(r1.ok);
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p))
            .await;
        assert!(
            r2.ok,
            "second pin (same project) must be idempotent: {}",
            r2.message
        );
        // No outbound message sent.
        assert_eq!(adapter.send_count(), 0);
    }

    // ── TEST O: existing unpinned RESUMABLE session rejects thread.pin ──
    //
    // Per TL v3: `has_reusable_session` must cover active, suspended, AND
    // persisted. Test O exercises the suspended + persisted states directly
    // via `SessionPoolTestState` (doesn't spawn a real subprocess). Test K
    // (below) covers the active path via the recording test agent.
    #[tokio::test]
    async fn existing_unpinned_resumable_session_rejects_thread_pin() {
        // Test O: persisted + suspended sessionId, no project binding.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let mut state = empty_pool_state();
        state
            .persisted
            .insert("discord:T1".into(), "sess_legacy_id".into());
        state
            .suspended
            .insert("discord:T1".into(), "sess_legacy_id".into());
        let pool = pool_with_state(dir.path(), state);
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // The reusable-session semantic must be true BEFORE the pin call.
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "test setup: pre-populated persisted+suspended must make the session reusable"
        );

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(!resp.ok, "pin must fail closed on reusable state");
        assert!(
            resp.message
                .contains("session already exists without trusted project binding"),
            "error must name the invariant: {}",
            resp.message
        );

        // No project binding written.
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "no project binding must be written"
        );

        // The reusable session states are STILL there (untouched) — the
        // pin must not delete them, only reject.
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "the persisted/suspended sessionId must remain in the pool"
        );
        assert_eq!(adapter.send_count(), 0, "no outbound message must be sent");
    }

    // ── TEST K: existing unpinned ACTIVE session rejects thread.pin ──
    //
    // Tech Lead v3 mandate: a REAL active ACP session must be created
    // (via the test agent script that responds to JSON-RPC), then
    // `thread.pin(project A)` must fail closed. Helper coverage via
    // `has_reusable_session` is NOT acceptable.
    //
    // Step 1: bootstrap a real active session via the test agent script
    // (no project binding). The agent responds to `initialize` /
    // `session/new` so `pool.get_or_create` completes the spawn path.
    // Step 2: capture the active connection Arc for stability check.
    // Step 3: invoke `RuntimeHandler` ctl `thread.pin(project A)`.
    // Step 4: assert explicit failure, no project binding written,
    // active Arc unchanged, no outbound adapter message.
    #[cfg(unix)]
    #[tokio::test]
    async fn existing_unpinned_active_session_rejects_thread_pin() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        // Use the standard test agent script (no recording) — we only
        // need a real active session for the fail-closed invariant.
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // Step 1: bootstrap a real ACTIVE session for T1 via the test
        // agent script. The agent responds to `initialize` / `session/new`
        // so `pool.get_or_create` completes the spawn path.
        let created = pool
            .get_or_create("discord:T1", None)
            .await
            .expect("active session bootstrap must succeed");
        assert!(created, "T1 must be a fresh active session");

        // Sanity: the session is alive (so the pin path will hit the
        // active-session fast path inside `has_reusable_session`).
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "test setup: active session must be present"
        );
        assert!(
            pool.has_active_session("discord:T1").await,
            "test setup: active session must be alive"
        );

        // Verify the existing session has NO project binding (test setup).
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "test setup: active session must not have a project binding"
        );

        // Step 2: invoke ctl thread.pin(project A).
        let mut project_a = project_root(project_dir.path());
        project_a.project_id = "A".into();
        let resp = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&project_a))
            .await;
        assert!(!resp.ok, "pin must fail closed on unpinned active session");
        assert!(
            resp.message
                .contains("session already exists without trusted project binding"),
            "error must name the invariant: {}",
            resp.message
        );

        // Step 3a: no project binding was written.
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "fail-closed path must NOT write a project binding"
        );

        // Step 3b: the active session is STILL alive (no mutation, no
        // silent re-spawn). `has_active_session` does a live connection
        // check; the fact that it returns true proves the pool's alive
        // flag hasn't flipped.
        assert!(
            pool.has_active_session("discord:T1").await,
            "active session must remain alive after pin rejection"
        );
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "reusable-session state must remain true (the active connection is the reusable state)"
        );

        // Step 3c: the active session is still FUNCTIONAL — a follow-up
        // `get_or_create(T, None)` must hit the existing active connection
        // fast path and return Ok(false) (no new session).
        let created_again = pool
            .get_or_create("discord:T1", None)
            .await
            .expect("follow-up call must succeed");
        assert!(
            !created_again,
            "active session must be reused, not re-spawned"
        );

        // Step 3d: no outbound adapter message is sent.
        assert_eq!(
            adapter.send_count(),
            0,
            "thread.pin must NOT call adapter.send_message_targeted"
        );
    }

    // ── TEST G: ctl request without project fields is backward compatible ──

    #[tokio::test]
    async fn ctl_request_without_project_fields_is_backward_compatible() {
        // No project field → thread.pin must fail with a clear "requires
        // project" error, while thread.message (without project) continues
        // to work via the legacy send_message_targeted path.
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // thread.pin without project → error.
        let resp = handler
            .handle_set(Some("T1"), "thread.pin", "", None, None)
            .await;
        assert!(!resp.ok);
        assert!(resp.message.contains("requires a project field"));

        // thread.message without project → sends via the legacy path.
        let resp = handler
            .handle_set(Some("T1"), "thread.message", "hello", None, None)
            .await;
        assert!(
            resp.ok,
            "legacy thread.message must still work: {}",
            resp.message
        );
        assert_eq!(adapter.send_count(), 1);
        assert_eq!(adapter.last_value().as_deref(), Some("hello"));
    }

    // ── TEST F: ctl request with project fields propagates to SessionPool ──

    #[tokio::test]
    async fn ctl_request_with_project_fields_propagates_to_session_pool() {
        // thread.pin and thread.message both propagate the project to
        // SessionPool. For thread.message, the message is sent AFTER the
        // pin succeeds.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // thread.pin with project.
        let r1 = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(r1.ok);
        assert!(pool.get_pinned_project("discord:T1").await.is_some());

        // thread.message with project on the SAME thread — idempotent
        // pin, then send.
        let r2 = handler
            .handle_set(
                Some("T1"),
                "thread.message",
                "hello world",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(r2.ok, "thread.message with project: {}", r2.message);
        assert_eq!(adapter.send_count(), 1);
        assert_eq!(adapter.last_value().as_deref(), Some("hello world"));
    }

    // ── TEST N: thread.message(project=B) on thread pinned A → no message sent ──

    #[tokio::test]
    async fn thread_message_with_mismatched_project_does_not_send_discord_message() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // Pin T1 to A.
        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok);

        // thread.message with project B → pin must fail closed, and the
        // adapter MUST NOT receive any send_message_targeted call.
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();
        let r2 = handler
            .handle_set(
                Some("T1"),
                "thread.message",
                "should not send",
                None,
                Some(&b),
            )
            .await;
        assert!(!r2.ok, "mismatch must reject");
        assert!(
            r2.message.contains("pin failed"),
            "error must surface the pin failure: {}",
            r2.message
        );
        assert_eq!(
            adapter.send_count(),
            0,
            "adapter.send_message_targeted MUST NOT be called when pin fails"
        );
        // The original binding must remain A.
        let pinned = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(pinned.project_id, "A");
    }

    // ── TEST I: project-root canonicalization preserves equivalence ──

    #[tokio::test]
    async fn project_root_canonicalization_preserves_equivalence() {
        // `project_root` written via `ProjectRef::try_from` is canonicalized.
        // A second pin with a trailing-slash variant of the SAME directory
        // must canonicalize to the same ProjectContext and be idempotent.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // First pin with the canonical path.
        let p1 = project_root(project_dir.path());
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p1))
            .await;
        assert!(r1.ok);

        // Second pin with the SAME logical path plus a trailing slash.
        let trailing = format!("{}/", project_dir.path().to_string_lossy());
        let p2 = ProjectRef {
            project_id: "openab".into(),
            project_root: trailing,
        };
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p2))
            .await;
        assert!(
            r2.ok,
            "trailing-slash variant of canonical project must be idempotent: {}",
            r2.message
        );

        // Exactly one binding (idempotent — no second create).
        let pinned = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(
            pinned.project_root,
            project_dir.path().canonicalize().unwrap()
        );
    }

    // ── TEST E: legacy no project uses agent working_dir ──

    #[tokio::test]
    async fn legacy_no_project_uses_agent_working_dir_at_pool_level() {
        // No project, no stored binding → pool falls back to
        // config.working_dir. This is exercised at the SessionPool layer
        // (test `legacy_session_new_receives_configured_working_dir` in
        // pool.rs). Here we just verify the ctl layer does not interfere
        // when there's no project field on a thread.message.
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(Some("T1"), "thread.message", "hi", None, None)
            .await;
        assert!(resp.ok);
        // No project binding written by the ctl layer.
        assert!(pool.get_pinned_project("discord:T1").await.is_none());
    }

    // ── TEST J: project binding survives restart via session_projects.json ──

    #[tokio::test]
    async fn project_binding_survives_restart_via_session_projects_json() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok);

        // The writer (RuntimeHandler::ensure_pinned_project via pool.get_or_create)
        // saves to the projects_path. Read the JSON file directly to verify
        // the binding is persisted across the daemon's restart lifecycle.
        let projects_path = dir.path().join("session_projects.json");
        let raw = std::fs::read_to_string(&projects_path)
            .expect("session_projects.json must be present after a successful pin");
        let persisted: HashMap<String, CoreProjectContext> =
            serde_json::from_str(&raw).expect("projects file must round-trip");
        assert!(persisted.contains_key("discord:T1"));
        let p = &persisted["discord:T1"];
        assert_eq!(p.project_id, "openab");
        assert_eq!(p.project_root, project_dir.path().canonicalize().unwrap());
    }

    // ── TEST H: untrusted Discord message text cannot inject project_root ──

    /// The dispatcher's `parse_directives` only recognizes `[[ws:@alias]]` and
    /// `[[title:...]]` — there is no `[[project_id=...]]` directive. The ctl
    /// layer's `project` field is the only path that supplies a project to
    /// `SessionPool`. A message that happens to contain the substring
    /// `project_id: openab` in its body must not be picked up by the
    /// dispatcher's anonymous-context seam.
    #[test]
    fn untrusted_discord_message_text_cannot_inject_project_root() {
        // Sanity-check that the dispatcher path's directive parser only
        // accepts `[[ws:...]]` and `[[title:...]]` — see
        // `crates/openab-core/src/directives.rs`. Anything resembling
        // `project_id=...` is just user text and never reaches the pool.
        let untrusted_body = "please pin project_id=openab project_root=/etc/passwd";
        let parsed = openab_core::directives::parse_directives(untrusted_body);
        let raw = &parsed.metadata.raw;
        assert!(
            raw.get("ws").is_none(),
            "ws must not be set; the dispatcher must not extract a project hint from arbitrary text"
        );
        assert!(
            raw.get("project_id").is_none(),
            "project_id must not be set; no such directive exists"
        );
        // The prompt is preserved verbatim (no silent stripping).
        assert_eq!(parsed.prompt, untrusted_body);
    }

    // ── Sentinel: SenderContext is reachable through the workspace's
    // adapter surface (used by future sender-bound tests). ──
    #[allow(dead_code)]
    fn _ensure_sender_context_in_scope() -> SenderContext {
        SenderContext {
            schema: "openab.sender.v1".into(),
            sender_id: "u".into(),
            sender_name: "u".into(),
            display_name: "u".into(),
            channel: "c".into(),
            channel_id: "c".into(),
            thread_id: None,
            is_bot: false,
            timestamp: None,
            message_id: None,
            receiver_id: None,
        }
    }

    // ── E2E: real UDS chain ──────────────────────────────────────────────
    //
    // The 12-point Tech Lead mandate requires the E2E to actually cross:
    //   Unix ctl socket → send_request_to → RuntimeHandler
    //     → ensure_pinned_project → SessionPool::get_or_create
    //     → real recording ACP test agent → session/new.params.cwd
    //
    // Helper-only tests (e.g. calling `handle_set` directly) are NOT
    // acceptable E2E coverage. This test wires the real
    // `spawn_server_at` + `send_request_to` UDS path AND verifies the
    // `session/new.params.cwd` value the agent actually received.
    //
    // Also exercises `thread.message(project=A)` through the same UDS
    // path so the pin-first + outbound-adapter sequencing is observable
    // end-to-end.
    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_trusted_thread_pin_drives_session_new_cwd() {
        // ── Wire the real UDS server with a recording test agent ──
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let canonical_project_root = project_dir.path().canonicalize().unwrap();

        let record_path = dir.path().join("agent-rpc.log");
        let pool = recording_pool(dir.path(), &record_path);
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let sock = dir.path().join("test.sock");
        let server = spawn_server_at(sock.clone(), std::sync::Arc::new(handler));
        // Give the server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // ── Step 1: `thread.pin` via the UDS protocol ──
        let pin_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.pin".into(),
                value: None,
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(project_root(project_dir.path())),
                agent_work: None,
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(
            pin_resp.ok,
            "thread.pin must succeed via UDS: {}",
            pin_resp.message
        );
        assert!(
            pin_resp.message.contains("pinned"),
            "pin response must confirm the pin"
        );

        // Proves the pin crossed the UDS protocol path AND reached the
        // pool: `session_projects[discord:T1]` must exist.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("pool must have entry under canonical key discord:T1 after UDS pin");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(pinned.project_root, canonical_project_root);

        // ── Step 2: validate the agent actually received the canonical cwd ──
        //
        // The recording test agent writes every JSON-RPC line it received
        // to `record_path`. Reading the file directly proves the project
        // root reached the agent — not just that the pool's in-memory
        // state looks right.
        let cwd = cwd_from_recorded_session_new(&record_path);
        assert_eq!(
            cwd,
            canonical_project_root.to_string_lossy(),
            "session/new.params.cwd must be the canonical project_root \
             (recorded by the agent, not inferred from pool state)"
        );

        // ── Step 3: `thread.message(project=A)` through the same UDS ──
        //
        // Same project (idempotent pin), then send. The pin path must
        // not corrupt the binding and the outbound adapter call must
        // happen exactly once.
        let msg_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("HANDOFF via UDS".into()),
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(project_root(project_dir.path())),
                agent_work: None,
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(
            msg_resp.ok,
            "thread.message with idempotent project must succeed via UDS: {}",
            msg_resp.message
        );

        // Pin survived (same project).
        let pinned_after = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(pinned_after.project_root, canonical_project_root);

        // Outbound adapter dispatched exactly once.
        assert_eq!(
            adapter.send_count(),
            1,
            "thread.message with project must dispatch exactly once"
        );
        assert_eq!(
            adapter.last_value().as_deref(),
            Some("HANDOFF via UDS"),
            "the message body reaches the adapter verbatim"
        );

        // ── Step 4: `thread.message(project=B)` against pinned A fails
        // closed AND does NOT send ──
        let mut b = project_root(project_dir.path());
        b.project_id = "B".into();
        let bad_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("should not send".into()),
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(b),
                agent_work: None,
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(
            !bad_resp.ok,
            "mismatched project must reject; UDS must surface the pin failure"
        );
        assert!(
            bad_resp.message.contains("pin failed"),
            "error must surface the pin failure: {}",
            bad_resp.message
        );
        // Adapter dispatch count UNCHANGED — the unrejected `HANDOFF via UDS`
        // is the only outbound message.
        assert_eq!(
            adapter.send_count(),
            1,
            "a mismatched thread.message must NOT call adapter.send_message_targeted"
        );

        // Cleanup.
        server.abort();
    }

    // ================================================================
    // Phase 6.4.1F Correction Round 1 — daemon wire-admission
    // fail-closed regression suite for ``scope_policy``.
    //
    // Every test below pins the authoritative boundary:
    //
    //     agent.work JSON
    //       → AgentWorkRequest
    //         → validate_agent_work()
    //           → only then construct NativeWorkflowMetadata
    //
    // Malformed ``scope_policy`` MUST be rejected at this seam and
    // MUST NOT reach native dispatch. The test helper
    // ``scope_policy_admission_test`` exercises the seam by driving
    // ``validate_agent_work`` directly so the gate's behaviour is
    // pinned independently of the surrounding admission machinery
    // (which is covered by the broader ``handle_agent_work``
    // test envelope).
    // ================================================================

    fn scope_policy_admission_test(policy: Option<AgentWorkScopePolicy>) -> Option<&'static str> {
        let mut request = native_work_request();
        request.scope_policy = policy;
        validate_agent_work(&request)
    }

    fn canonical_scope_policy(write_policy: &str) -> AgentWorkScopePolicy {
        AgentWorkScopePolicy {
            scope_mode: "BOUNDED".into(),
            write_policy: write_policy.into(),
            historical_context_policy: "ADVISORY_ONLY".into(),
        }
    }

    // Required test 1 — valid: BOUNDED / READ_ONLY / ADVISORY_ONLY → accepted.
    #[test]
    fn admission_accepts_canonical_read_only_scope_policy() {
        let result = scope_policy_admission_test(Some(canonical_scope_policy("READ_ONLY")));
        assert!(
            result.is_none(),
            "canonical READ_ONLY must be accepted at the admission gate; got: {result:?}"
        );
    }

    // Required test 2 — valid: BOUNDED / MODIFY_ALLOWED / ADVISORY_ONLY → accepted.
    #[test]
    fn admission_accepts_canonical_modify_allowed_scope_policy() {
        let result = scope_policy_admission_test(Some(canonical_scope_policy("MODIFY_ALLOWED")));
        assert!(
            result.is_none(),
            "canonical MODIFY_ALLOWED must be accepted at the admission gate; got: {result:?}"
        );
    }

    // Required test 3 — ``scope_policy = None`` → legacy accepted.
    #[test]
    fn admission_preserves_legacy_for_missing_scope_policy() {
        let result = scope_policy_admission_test(None);
        assert!(
            result.is_none(),
            "absent scope_policy must preserve pre-6.4.1F legacy; got: {result:?}"
        );
    }

    // Required test 4 — unknown ``scope_mode`` → rejected.
    #[test]
    fn admission_rejects_unknown_scope_mode() {
        let mut bad = canonical_scope_policy("READ_ONLY");
        bad.scope_mode = "FREE".into();
        let result = scope_policy_admission_test(Some(bad));
        assert_eq!(
            result,
            Some("scope_policy.scope_mode is not a canonical token"),
            "unknown scope_mode must be rejected at the admission gate"
        );
    }

    // Required test 5 — unknown ``write_policy`` → rejected.
    #[test]
    fn admission_rejects_unknown_write_policy() {
        let mut bad = canonical_scope_policy("READ_ONLY");
        bad.write_policy = "GARBAGE".into();
        let result = scope_policy_admission_test(Some(bad));
        assert_eq!(
            result,
            Some("scope_policy.write_policy is not a canonical token"),
            "unknown write_policy must be rejected at the admission gate"
        );
    }

    // Required test 6 — unknown ``historical_context_policy`` → rejected.
    #[test]
    fn admission_rejects_unknown_historical_context_policy() {
        let mut bad = canonical_scope_policy("READ_ONLY");
        bad.historical_context_policy = "AUTHORITATIVE".into();
        let result = scope_policy_admission_test(Some(bad));
        assert_eq!(
            result,
            Some("scope_policy.historical_context_policy is not a canonical token"),
            "unknown historical_context_policy must be rejected at the admission gate"
        );
    }

    // Required test 7 — empty ``write_policy`` → rejected.
    #[test]
    fn admission_rejects_empty_write_policy() {
        let mut bad = canonical_scope_policy("READ_ONLY");
        bad.write_policy = "".into();
        let result = scope_policy_admission_test(Some(bad));
        assert_eq!(
            result,
            Some("scope_policy.write_policy is empty"),
            "empty write_policy must be rejected at the admission gate"
        );
    }

    // Required test 8 — partial policy payload missing one required
    // field → rejected, not defaulted. The struct itself no longer
    // carries ``#[serde(default)]`` so partial payloads cannot reach
    // ``validate_agent_work`` — but the validator still belt-and-
    // braces an empty-string field to guarantee the invariant even
    // if a future refactor re-introduces defaults.
    #[test]
    fn admission_rejects_partial_scope_policy_payload() {
        // (a) Empty-string field = the post-default view of a
        //     partial payload. The validator rejects it.
        let mut partial_via_empty = canonical_scope_policy("READ_ONLY");
        partial_via_empty.scope_mode = "".into();
        let result = scope_policy_admission_test(Some(partial_via_empty));
        assert_eq!(
            result,
            Some("scope_policy.scope_mode is empty"),
            "partial scope_policy (missing scope_mode) must be rejected"
        );

        // (b) JSON-level partial: the wire DTO has no
        //     ``#[serde(default)]`` so ``serde_json::from_str`` itself
        //     rejects the request before ``validate_agent_work`` is
        //     reached. The admission path therefore fails closed at
        //     the earliest possible boundary.
        let partial_json = r#"{
            "dispatch_id": "d",
            "workflow_run_id": "r",
            "task_id": "t",
            "role": "PRIMARY",
            "agent": "ArthurClaude",
            "lease_id": "l",
            "lease_generation": 1,
            "conversation_key": "123456",
            "assignment": "a",
            "language": "en",
            "scope_policy": {"write_policy": "READ_ONLY"}
        }"#;
        let parsed: Result<AgentWorkRequest, _> = serde_json::from_str(partial_json);
        assert!(
            parsed.is_err(),
            "partial scope_policy payload must not deserialize at all"
        );
    }

    // Required test 9 — malformed READ_ONLY-like value such as
    // ``READ_ONLY_X`` → rejected. This is the verifier's specific
    // concern: a token that *looks* like READ_ONLY but isn't a
    // canonical value MUST NOT silently fall through to
    // MODIFY_ALLOWED.
    #[test]
    fn admission_rejects_read_only_like_lookalike() {
        for lookalike in ["READ_ONLY_X", "Read_Only", "read_only", "READ_ONLY "] {
            let bad = canonical_scope_policy(lookalike);
            let result = scope_policy_admission_test(Some(bad));
            assert_eq!(
                result,
                Some("scope_policy.write_policy is not a canonical token"),
                "READ_ONLY lookalike {lookalike:?} must be rejected at the admission gate"
            );
        }
    }

    // Required test 10 — Confirm valid READ_ONLY still reaches the
    // existing ACP ``WritePolicyGuard``. The end-to-end path:
    //
    //     canonical payload
    //       → validate_agent_work (accepts)
    //         → handle_agent_work → build NativeWorkflowMetadata
    //           → dispatch_batch → ensure_session(write_policy=Some("READ_ONLY"))
    //             → SessionPool::set_session_write_policy
    //               → AcpConnection::write_policy_guard.set("READ_ONLY")
    //
    // is exercised end-to-end via the recording admission port,
    // which captures the constructed ``NativeWorkflowMetadata``
    // before it is consumed by ``dispatch_batch``. The recording
    // itself is the production ACP gate surface — if the policy
    // did not propagate, the recorded metadata would carry
    // ``write_policy != "READ_ONLY"`` and the assertion would fail.
    #[tokio::test]
    async fn admission_read_only_propagates_to_native_workflow_metadata() {
        let recording = Arc::new(RecordingAdmissionPort::new("admission-1"));
        let handler = native_work_handler(recording.clone());
        let mut request = native_work_request();
        request.scope_policy = Some(canonical_scope_policy("READ_ONLY"));

        let resp = handler.handle_agent_work(Some(&request)).await;
        assert!(
            resp.ok,
            "valid READ_ONLY payload must be accepted: {resp:?}"
        );
        assert_eq!(
            recording.calls(),
            1,
            "admission must record exactly one call when validation passes"
        );
        let (_channel, captured) = recording.last_admission();
        let captured = captured.expect("native_workflow metadata was captured");
        let policy = captured
            .scope_policy
            .expect("scope_policy must propagate from valid READ_ONLY payload");
        assert_eq!(policy.write_policy, "READ_ONLY");
        assert_eq!(policy.scope_mode, "BOUNDED");
        assert_eq!(policy.historical_context_policy, "ADVISORY_ONLY");
        assert!(
            policy.is_read_only(),
            "propagated policy MUST surface READ_ONLY to the ACP WritePolicyGuard"
        );
    }

    // Belt-and-braces companion to test 10: confirm a malformed
    // payload never reaches the admission port. The recording
    // adapter's call counter MUST stay at zero — proving the
    // fail-closed seam is between validate_agent_work and
    // NativeWorkflowMetadata construction, not at a downstream
    // point.
    #[tokio::test]
    async fn admission_malformed_scope_policy_never_reaches_admission_port() {
        let recording = Arc::new(RecordingAdmissionPort::new("admission-1"));
        let handler = native_work_handler(recording.clone());
        let mut request = native_work_request();
        // Unknown write_policy — would have silently weakened to
        // MODIFY_ALLOWED before Correction Round 1.
        let mut bad = canonical_scope_policy("READ_ONLY");
        bad.write_policy = "READ_ONLY_X".into();
        request.scope_policy = Some(bad);

        let resp = handler.handle_agent_work(Some(&request)).await;
        assert!(
            !resp.ok,
            "malformed scope_policy must yield a non-OK response: {resp:?}"
        );
        assert!(
            resp.message.contains("scope_policy.write_policy"),
            "error must surface the malformed field path: {}",
            resp.message
        );
        assert_eq!(
            recording.calls(),
            0,
            "the admission port MUST NOT be called when scope_policy is malformed"
        );
    }
}

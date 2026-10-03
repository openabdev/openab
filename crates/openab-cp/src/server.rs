//! WebSocket server: authentication at upgrade, mandatory `cp/register`
//! first frame, then frame dispatch to registry/policy/router.
//!
//! Auth: the runtime presents its key as `Authorization: Bearer <key>` on the
//! upgrade request. Keys never appear in URLs (avoids access-log leakage).
//!
//! Resource bounds: the WS transport enforces `max_frame_bytes` before
//! parsing; each connection's outbound queue is bounded — a peer that cannot
//! drain it is treated as disconnected — and every outbound write is itself
//! bounded by `write_timeout_secs` and raced against the CP's close signal, so
//! a peer that stops reading cannot park the connection task (and with it the
//! identity's connection quota and its in-flight delegations).
//!
//! Admission bounds: authentication alone is not a bound. Every connection
//! holds a per-identity slot from the upgrade until it ends (`ConnPermit`,
//! released on every exit path), and must complete `cp/register` within
//! `register_timeout_secs` or be closed.
//!
//! CP-initiated closes carry meaning: both the registration-timeout close and
//! the lease-expiry close are WS code 1008 (policy violation) with a short
//! reason (`registration timeout` / `lease expired`), and an over-quota
//! upgrade is refused with HTTP 503 naming the quota. A client can therefore
//! tell "I misbehaved / my lease lapsed" from a transport-level drop without
//! consulting CP logs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router as AxumRouter;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{AgentIdentity, CpConfig};
use crate::events::EventHub;
use crate::proto::{
    codes, methods, AgentSummary, AgentType, CancelParams, CpEvent, DelegateParams,
    DelegateResultParams, DeregisterReason, ErrorObject, JsonRpcErrorResponse, JsonRpcMessage,
    JsonRpcResponse, ListAgentsResult, RegisterAck, RegisterParams, PROTOCOL_VERSION,
};
use crate::registry::{outbound_channel, shutdown_signal, Instance, Registry, ShutdownTx};
use crate::router::{CompleteOutcome, DelegateOutcome, Router};

pub struct AppState {
    pub cfg: CpConfig,
    pub registry: Registry,
    pub router: Router,
    /// Observer fan-out (`cp/event`) with per-namespace sequence numbers.
    pub events: EventHub,
    rpc_id: AtomicU64,
    /// Live connections per identity (`namespace/name`), counted from the
    /// upgrade so pre-registration sockets are bounded too.
    conns: Mutex<BTreeMap<String, u32>>,
    /// Process-wide shutdown broadcast, latched once by
    /// [`graceful_shutdown`] on SIGTERM/SIGHUP/SIGINT. Every connection task
    /// subscribes — including pre-registration sockets, which the
    /// per-connection signal held by the registry cannot reach.
    shutdown: ShutdownTx,
    /// When the current drain expires, latched together with `shutdown`.
    /// Every budget in the drain is a remainder of this one instant.
    shutdown_deadline: Mutex<Option<Instant>>,
}

impl AppState {
    pub fn new(cfg: CpConfig) -> Self {
        Self {
            events: EventHub::new(&cfg),
            cfg,
            registry: Registry::new(),
            router: Router::new(),
            rpc_id: AtomicU64::new(1),
            conns: Mutex::new(BTreeMap::new()),
            shutdown: shutdown_signal(),
            shutdown_deadline: Mutex::new(None),
        }
    }

    pub fn next_rpc_id(&self) -> u64 {
        self.rpc_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Subscribe to the process-wide shutdown signal: resolves to the
    /// shutdown reason once [`AppState::begin_shutdown`] latches it.
    pub fn shutdown_rx(&self) -> watch::Receiver<Option<&'static str>> {
        self.shutdown.subscribe()
    }

    /// Latch the shutdown reason: every live connection task closes, and a
    /// connection upgraded mid-drain sees the latched reason at subscribe
    /// time. The drain deadline is latched with it, so every connection
    /// budgets against the SAME instant rather than each starting a fresh
    /// `shutdown_drain_secs` when it happens to leave its main loop — a
    /// connection that noticed the signal late must not be granted a longer
    /// life than the drain it is part of.
    pub fn begin_shutdown(&self, reason: &'static str) {
        let deadline = Instant::now() + Duration::from_secs(self.cfg.shutdown_drain_secs);
        *self.shutdown_deadline.lock() = Some(deadline);
        self.shutdown.send_replace(Some(reason));
    }

    /// The instant the current drain expires. `None` until the CP begins
    /// shutting down.
    fn shutdown_deadline(&self) -> Option<Instant> {
        *self.shutdown_deadline.lock()
    }

    /// Whether the CP has begun draining. Distinguishes "this peer stopped
    /// reading" from "the CP is leaving" when a frame cannot be delivered —
    /// the two deserve different explanations.
    pub fn shutting_down(&self) -> bool {
        self.shutdown_deadline.lock().is_some()
    }

    /// Resolves when no live connection permits remain — every connection
    /// task (registered or still pre-registration) has exited. Polled:
    /// drain progress is a wait on an ever-shrinking set, not a precise
    /// signal.
    pub async fn connections_drained(&self) {
        while !self.conns.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Take a connection slot for `identity`, or `None` when the identity is
    /// already at `max_connections_per_identity`. The returned guard releases
    /// the slot on drop — including on every early return and on an upgrade
    /// that never completes.
    pub fn try_acquire_conn(self: &Arc<Self>, identity: &AgentIdentity) -> Option<ConnPermit> {
        let key = format!("{}/{}", identity.namespace, identity.name);
        let mut g = self.conns.lock();
        let n = g.entry(key.clone()).or_insert(0);
        if *n >= self.cfg.max_connections_per_identity {
            return None;
        }
        *n += 1;
        Some(ConnPermit {
            state: Arc::clone(self),
            key,
        })
    }

    /// Live connection count for an identity (`namespace/name`).
    pub fn conn_count(&self, logical_id: &str) -> u32 {
        self.conns.lock().get(logical_id).copied().unwrap_or(0)
    }
}

/// RAII connection slot. Dropping it frees the identity's quota; it is never
/// released explicitly, so no early return can leak it.
pub struct ConnPermit {
    state: Arc<AppState>,
    key: String,
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        let mut g = self.state.conns.lock();
        if let Some(n) = g.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.remove(&self.key);
            }
        }
    }
}

pub fn app(state: Arc<AppState>) -> AxumRouter {
    AxumRouter::new()
        .route("/cp", get(ws_handler))
        .route("/health", get(health))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// Reason string on the close frame sent when `cp/register` never arrived.
pub const REASON_REGISTER_TIMEOUT: &str = "registration timeout";
/// Reason string on the close frame sent when the CP drops a registration
/// because its lease elapsed.
pub const REASON_LEASE_EXPIRED: &str = "lease expired";
/// Reason string on the close frame sent when a peer's bounded outbound
/// queue refused a terminal frame: per the queue contract the peer is
/// treated as disconnected, not buffered.
pub const REASON_BACKPRESSURE: &str = "outbound queue overflow";

/// A CP-initiated close that states why. Code 1008 (policy violation) plus a
/// short reason, so a client can distinguish "the CP closed me on purpose"
/// from a transport-level drop and act on it (re-register vs. plain retry).
fn policy_close(reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::POLICY,
        reason: reason.into(),
    }))
}

/// Reason every connection gets on its close frame when the CP shuts down
/// (SIGTERM/SIGHUP/SIGINT), and carried on the terminal/cancel frames
/// synthesized for in-flight delegations so a runtime can tell "the CP is
/// going away" from "my peer disconnected".
pub const REASON_SHUTDOWN: &str = "control plane shutting down";

/// A CP-initiated close for process shutdown: code 1012 (service restart —
/// the client should reconnect) rather than the 1008 the misbehaviour
/// closes above use, because nothing about a deploy is a policy violation.
fn restart_close(reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::RESTART,
        reason: reason.into(),
    }))
}

/// The two signals that can terminate a connection: the per-connection one
/// held by the registry entry (lease expiry, terminal-frame backpressure)
/// and the process-wide one latched on [`AppState`] when the CP begins
/// draining. The process signal is subscribed at upgrade time so
/// pre-registration sockets — which the registry cannot see yet — close
/// with a reason too.
struct CloseWatch {
    conn: watch::Receiver<Option<&'static str>>,
    process: watch::Receiver<Option<&'static str>>,
}

impl CloseWatch {
    /// Wait for the next transition on either signal. `watch` semantics: a
    /// value already seen is not reported again, so a caller that has
    /// consumed a reason keeps writing its close frame undisturbed.
    async fn changed(&mut self) {
        tokio::select! {
            _ = self.conn.changed() => {}
            _ = self.process.changed() => {}
        }
    }

    /// The process-wide shutdown reason, when the CP is draining.
    fn process_reason(&self) -> Option<&'static str> {
        *self.process.borrow()
    }

    /// The close reason set on either signal, if any; the process reason
    /// wins because "the CP is going away" is the actionable fact when both
    /// are latched (e.g. a lease sweep racing the drain).
    fn reason(&self) -> Option<&'static str> {
        (*self.process.borrow()).or(*self.conn.borrow())
    }
}

/// Why a bounded write did not complete. Either way the connection ends.
enum WriteStop {
    /// Transport error, or the peer did not accept the frame within
    /// `write_timeout_secs` — a peer that cannot be written to is treated as
    /// disconnected, exactly like one that cannot drain its queue.
    Disconnected,
    /// The CP asked this connection to close while the write was pending,
    /// with the reason to put on the close frame (`None` if the signal
    /// carried none).
    Shutdown(Option<&'static str>),
}

/// Send one frame with a bound on how long it may block, while remaining
/// responsive to the CP's own close signal.
///
/// Both properties are load-bearing. `sink.send().await` inside a `select!`
/// arm body is NOT cancelled by the other arms, so an unbounded write parks
/// the whole connection task: it stops reading inbound frames, stops
/// observing the shutdown watch, and — because [`ConnPermit`] is released
/// only when the task returns — pins its identity's connection quota. A peer
/// with a closed TCP receive window or a half-open connection could hold
/// those slots indefinitely and no lease expiry could reclaim them, since
/// lease expiry works by signalling this very task.
///
/// A cancelled or timed-out write can leave a partially written frame on the
/// wire; that is acceptable precisely because both outcomes end the
/// connection (the caller breaks to teardown, dropping the socket).
///
/// `biased` polling order matters for the drain path: the caller has
/// already dequeued `msg` from the outbound channel, so a close signal that
/// wins a fair race would destroy a frame that was ready to write. The send
/// is tried first — an immediately-writable socket completes it and the
/// close signal is observed on the next call — while a write that cannot
/// finish now still pends and is interrupted by the signal or the timeout.
async fn send_bounded(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: Message,
    write_timeout: Duration,
    close: &mut CloseWatch,
) -> Result<(), WriteStop> {
    tokio::select! {
        biased;
        sent = tokio::time::timeout(write_timeout, sink.send(msg)) => match sent {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(WriteStop::Disconnected),
            Err(_) => Err(WriteStop::Disconnected),
        },
        _ = close.changed() => Err(WriteStop::Shutdown(close.reason())),
    }
}

/// Send the LAST frame a connection will ever get — a close frame, or a
/// terminal error response written just before teardown. Unlike
/// [`send_bounded`] this is raced only against `write_timeout`, never the
/// close watch: the connection is already committed to ending, and a second
/// signal latching mid-write (e.g. a lease sweep landing during the
/// shutdown drain, or the process signal during a per-connection close)
/// must not eat the frame the peer was promised. A peer that has stopped
/// reading is still bounded by `write_timeout`, so it cannot hold teardown
/// longer than that.
async fn send_final_frame(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: Message,
    write_timeout: Duration,
) {
    let _ = tokio::time::timeout(write_timeout, sink.send(msg)).await;
}

/// Budget for the last frame of a connection that is ending: the write timeout,
/// clipped to what is left of the drain once the CP is shutting down.
///
/// The main-loop teardown already spends the remaining drain directly; this
/// covers the paths that never reach the main loop — a socket caught mid-upgrade
/// by the drain, a pre-registration close, a refused register frame. Without it
/// those writes could still hold their task for a full `write_timeout_secs`
/// (30s by default) after a 5s drain expired, so `shutdown_drain_secs` would
/// not really be the ceiling the docs promise.
fn final_frame_budget(state: &AppState, write_timeout: Duration) -> Duration {
    match state.shutdown_deadline() {
        Some(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .min(write_timeout),
        None => write_timeout,
    }
}

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let identity = match key.and_then(|k| state.cfg.identity_for_key(k)) {
        Some(id) => id.clone(),
        None => {
            warn!("WS rejected: missing or unknown auth key");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };
    // Per-identity connection quota, taken before the upgrade so an
    // over-quota peer is refused at the HTTP layer.
    let permit = match state.try_acquire_conn(&identity) {
        Some(p) => p,
        None => {
            warn!(
                agent = %format!("{}/{}", identity.namespace, identity.name),
                max = state.cfg.max_connections_per_identity,
                "WS rejected: identity is at its connection quota"
            );
            // Name the quota in the body: without it a client cannot tell an
            // exhausted quota from an overloaded CP, and both are 503.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "identity is at its connection quota (max_connections_per_identity = {})\n",
                    state.cfg.max_connections_per_identity
                ),
            )
                .into_response();
        }
    };
    let max_frame = state.cfg.max_frame_bytes;
    ws.max_message_size(max_frame)
        .max_frame_size(max_frame)
        .on_upgrade(move |socket| handle_connection(state, socket, identity, permit))
}

async fn handle_connection(
    state: Arc<AppState>,
    socket: WebSocket,
    identity: AgentIdentity,
    // Held for the connection's whole lifetime; dropped here on every exit
    // path, including the early returns below.
    _permit: ConnPermit,
) {
    let (mut sink, mut stream) = socket.split();
    let write_timeout = Duration::from_secs(state.cfg.write_timeout_secs);

    // Shutdown signals so the CP can close this socket when it drops the
    // registration on its own initiative (lease expiry), must terminate the
    // connection (terminal-frame backpressure), or the whole process is
    // draining (SIGTERM/SIGHUP/SIGINT via `graceful_shutdown`).
    //
    // The per-connection sender is created BEFORE the registration read —
    // and therefore before the registry ever holds a clone — so no signal
    // can be missed, and every write in this task, registration-phase writes
    // included, can be raced against either signal. The senders are kept
    // alive here for the whole connection: closing is driven by an explicit
    // signal, never by the registry happening to drop its side.
    let shutdown = shutdown_signal();
    let mut close = CloseWatch {
        conn: shutdown.subscribe(),
        process: state.shutdown_rx(),
    };

    // A socket that finished its upgrade while the CP was already draining
    // must not register into a dying process: close it immediately. `watch`
    // only reports transitions that postdate the subscription, so the
    // already-latched reason is checked once explicitly.
    if let Some(reason) = close.process_reason() {
        send_final_frame(
            &mut sink,
            restart_close(reason),
            final_frame_budget(&state, write_timeout),
        )
        .await;
        return;
    }

    // --- Registration: mandatory first frame, within a deadline ---
    // An authenticated peer must not be able to park idle sockets: pings keep
    // the transport alive but do not extend this deadline.
    let register = match tokio::select! {
        res = tokio::time::timeout(
            Duration::from_secs(state.cfg.register_timeout_secs),
            async {
                loop {
                    match stream.next().await {
                        Some(Ok(Message::Text(text))) => return Some(text),
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                        _ => return None,
                    }
                }
            },
        ) => res,
        _ = close.changed() => {
            // Shutdown while still pre-registration: no registry entry or
            // in-flight delegation exists to tear down — just tell the peer
            // why its socket is closing. Only the process signal can latch
            // before registration (the per-connection sender has no owner
            // yet), but the frame is still picked by which signal fired.
            let frame = match close.process_reason() {
                Some(reason) => restart_close(reason),
                None => policy_close(close.reason().unwrap_or(REASON_SHUTDOWN)),
            };
            send_final_frame(&mut sink, frame, final_frame_budget(&state, write_timeout)).await;
            return;
        }
    } {
        Ok(Some(text)) => text,
        Ok(None) => {
            warn!(agent = %identity.name, "connection closed before registration");
            return;
        }
        Err(_) => {
            warn!(
                agent = %format!("{}/{}", identity.namespace, identity.name),
                timeout_secs = state.cfg.register_timeout_secs,
                "no cp/register within the registration deadline — closing"
            );
            send_final_frame(
                &mut sink,
                policy_close(REASON_REGISTER_TIMEOUT),
                final_frame_budget(&state, write_timeout),
            )
            .await;
            return;
        }
    };
    let (reg, reg_rpc_id) = match parse_register(&register, &identity) {
        Ok(ok) => ok,
        Err((id, err)) => {
            let resp = JsonRpcErrorResponse::new(id, err);
            send_final_frame(
                &mut sink,
                Message::Text(serde_json::to_string(&resp).expect("serializable").into()),
                final_frame_budget(&state, write_timeout),
            )
            .await;
            return;
        }
    };

    // Outbound channel for this connection. Bounded twice: a peer that cannot
    // drain OUTBOUND_QUEUE frames — or that has accumulated
    // max_outbound_queue_bytes of them — is disconnected, not buffered.
    let (tx, mut rx) = outbound_channel(state.cfg.max_outbound_queue_bytes);

    // The advertised budget is self-asserted and therefore always clamped:
    // by this identity's own cap when set, otherwise by the global default.
    let effective_max = state
        .cfg
        .effective_max_sessions(&identity, reg.max_delegated_sessions);
    // The registry assigns the CP-generated handle: ownership
    // and teardown never key on the client-supplied instance_id.
    // Observers are additionally bounded per namespace: fan-out does
    // bounded per-observer work inside the delegation path's in-flight
    // critical section, so the observer count is a configured latency
    // budget, not an open-ended population.
    let handle = match state.registry.register_conn_capped(
        Instance {
            handle: 0,
            namespace: identity.namespace.clone(),
            name: identity.name.clone(),
            agent_type: identity.agent_type.clone(),
            instance_id: reg.instance_id.clone(),
            labels: reg.labels.clone(),
            max_delegated_sessions: effective_max,
            active_sessions: 0,
            registered_at: Instant::now(),
            last_heartbeat: Instant::now(),
            tx: tx.clone(),
        },
        Arc::clone(&shutdown),
        state.cfg.max_observers_per_namespace,
    ) {
        Ok(h) => h,
        Err(current) => {
            warn!(
                agent = %format!("{}/{}", identity.namespace, identity.name),
                observers = current,
                max = state.cfg.max_observers_per_namespace,
                "registration refused: namespace is at its observer cap"
            );
            let resp = JsonRpcErrorResponse::new(
                reg_rpc_id,
                ErrorObject::new(
                    codes::SATURATED,
                    format!(
                        "namespace is at its observer cap \
                         (max_observers_per_namespace = {}); retry later or \
                         raise the cap",
                        state.cfg.max_observers_per_namespace
                    ),
                ),
            );
            send_final_frame(
                &mut sink,
                Message::Text(serde_json::to_string(&resp).expect("serializable").into()),
                final_frame_budget(&state, write_timeout),
            )
            .await;
            return;
        }
    };
    // From here on, teardown is owned by an RAII guard rather than the return
    // path: a panic anywhere below (the `expect("serializable")` sites are on
    // production paths) would otherwise skip deregistration and leave this
    // instance's in-flight delegations — and the capacity they reserve on
    // OTHER instances — pinned until the lease expires. The guard runs on both
    // the normal return and an unwind, and is the ONLY caller of `teardown`
    // here, so the two paths cannot diverge.
    let _registered = RegistrationGuard {
        state: Arc::clone(&state),
        handle,
        identity: identity.clone(),
    };
    info!(
        agent = %format!("{}/{}", identity.namespace, identity.name),
        instance = %reg.instance_id,
        handle,
        r#type = %identity.agent_type,
        max_sessions = effective_max,
        "registered"
    );

    // Ack. The CP-generated handle is intentionally not disclosed.
    let ack = RegisterAck {
        protocol_version: PROTOCOL_VERSION,
        heartbeat_interval_secs: state.cfg.heartbeat_interval_secs,
        lease_expiry_secs: state.cfg.lease_expiry_secs,
        effective_max_delegated_sessions: effective_max,
    };
    let resp = JsonRpcResponse::new(
        reg_rpc_id,
        serde_json::to_value(&ack).expect("serializable"),
    );
    if let Err(stop) = send_bounded(
        &mut sink,
        Message::Text(serde_json::to_string(&resp).expect("serializable").into()),
        write_timeout,
        &mut close,
    )
    .await
    {
        // CP-initiated closes carry meaning even here: if the CP signalled
        // this connection while the ack was in flight, still tell the client
        // why before tearing down — a restart frame when the whole process
        // is draining, a policy frame when this connection alone is ending.
        if let WriteStop::Shutdown(_) = stop {
            let frame = match close.process_reason() {
                Some(reason) => Some(restart_close(reason)),
                None => close.reason().map(policy_close),
            };
            if let Some(frame) = frame {
                send_final_frame(&mut sink, frame, final_frame_budget(&state, write_timeout)).await;
            }
        }
        // `_registered` runs teardown on the way out.
        return;
    }

    // The lobby learns about every arrival — observers included, so one
    // lobby client sees the others. Announced only after a successful ack:
    // if the ack send fails, teardown emits an `agent_deregistered` with no
    // matching `agent_registered` — roster clients must treat removal of an
    // unknown agent as a no-op (they may have joined mid-stream anyway).
    announce_registration(&state, handle);

    // --- Main loop: interleave inbound frames, outbound channel, shutdown ---
    //
    // Every write goes through `send_bounded`: a `select!` arm body is not
    // cancelled by the other arms, so an unbounded write here would stop this
    // task from observing the shutdown watch and from releasing its
    // `ConnPermit` — see `send_bounded`.
    let mut cp_close_reason: Option<&'static str> = None;
    // A macro, not a closure: each expansion borrows `sink` only for the
    // duration of its own arm body, and `break` acts on the loop below.
    macro_rules! write_or_break {
        ($msg:expr) => {
            match send_bounded(&mut sink, $msg, write_timeout, &mut close).await {
                Ok(()) => {}
                Err(WriteStop::Shutdown(reason)) => {
                    cp_close_reason = reason;
                    break;
                }
                Err(WriteStop::Disconnected) => {
                    warn!(
                        handle,
                        timeout_secs = state.cfg.write_timeout_secs,
                        "outbound write failed or exceeded write_timeout_secs — \
                         treating the peer as disconnected"
                    );
                    break;
                }
            }
        };
    }
    loop {
        tokio::select! {
            // The CP dropped this registration (lease expiry), must
            // terminate the connection (terminal-frame backpressure), or the
            // process is shutting down: the socket must go too. Keeping it
            // open would leave a connection whose every frame hits an absent
            // registry entry and which can never re-register, since
            // registration is first-frame-only. Closing lets the client
            // reconnect, re-authenticate, and register again.
            _ = close.changed() => {
                cp_close_reason = close.reason();
                break;
            }
            outbound = rx.recv() => {
                match outbound {
                    Some(text) => write_or_break!(Message::Text(text.into())),
                    None => break,
                }
            }
            inbound = stream.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(reply) = handle_frame(&state, handle, &text) {
                            write_or_break!(Message::Text(reply.into()));
                        }
                    }
                    Some(Ok(Message::Ping(p))) => write_or_break!(Message::Pong(p)),
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // binary/pong ignored
                    Some(Err(e)) => {
                        warn!(handle, err = %e, "WS error");
                        break;
                    }
                }
            }
        }
    }

    // Process shutdown — SIGTERM/SIGHUP/SIGINT — is distinguished from a
    // per-connection termination by which signal latched. On the process
    // path the outbound queue is flushed FIRST: the synthesized terminals
    // and cancels that `graceful_shutdown` queued must reach the wire ahead
    // of the close frame. `rx.close()` refuses any further enqueue, so the
    // loop ends on its own once the backlog is flushed.
    //
    // Every budget below is a remainder of ONE instant — the deadline latched
    // when the CP began draining — not a fresh `shutdown_drain_secs` per
    // connection and not a fresh one per write. That is what makes the
    // documented bound true: `shutdown_drain_secs` covers the whole drain,
    // the flush and the final close write included.
    if let Some(reason) = close.process_reason() {
        rx.close();
        let deadline = state.shutdown_deadline().unwrap_or_else(Instant::now);
        while let Some(text) = rx.recv().await {
            let budget = deadline.saturating_duration_since(Instant::now());
            if budget.is_zero() {
                break;
            }
            if !matches!(
                tokio::time::timeout(budget.min(write_timeout), sink.send(text.into())).await,
                Ok(Ok(()))
            ) {
                return; // the peer is gone — teardown runs via the guard
            }
        }
        info!(
            agent = %format!("{}/{}", identity.namespace, identity.name),
            handle,
            reason,
            "closing connection: control plane shutting down"
        );
        // Bounded like every other write — a peer that has stopped reading
        // must not be able to hold teardown (and its quota slot) by refusing
        // to accept the close frame — but by what is LEFT of the drain, not by
        // a fresh timeout: a stalled peer cannot push the process's exit past
        // the budget the operator configured. The write is NOT raced against
        // the close watch: a second signal latching mid-write (a lease sweep
        // landing mid-drain) must not eat the promised close frame.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            send_final_frame(
                &mut sink,
                restart_close(reason),
                remaining.min(write_timeout),
            )
            .await;
        } else {
            warn!(
                handle,
                "drain deadline elapsed before this connection's close frame could be written"
            );
        }
    } else if let Some(reason) = cp_close_reason {
        info!(
            agent = %format!("{}/{}", identity.namespace, identity.name),
            handle,
            reason,
            "closing connection at the CP's request"
        );
        // Bounded like every other write: a peer that has stopped reading must
        // not be able to delay teardown (and its quota slot) by refusing to
        // accept the close frame. Not raced against the close watch either:
        // the process signal may latch mid-write while this frame is still
        // the only explanation the peer will get.
        send_final_frame(&mut sink, policy_close(reason), write_timeout).await;
    }

    // Teardown runs here, when `_registered` drops — on this path and on an
    // unwind alike.
}

/// RAII owner of a *registered* connection's CP-side state.
///
/// Scoped to the registered lifetime: constructed immediately after
/// `register_conn`, dropped when the connection task returns **or unwinds**.
/// [`ConnPermit`] already made the identity's connection quota panic-safe;
/// this does the same for the registry entry and the in-flight rows, which a
/// panic between registration and return would otherwise leave for the lease
/// sweeper — up to `lease_expiry_secs` of capacity reserved on *other*
/// instances for delegations nobody is serving any more.
///
/// [`teardown`] is idempotent by construction, which is what makes a guard
/// safe here: `deregister` is keyed by handle and returns `None` for an
/// already-removed entry, and `fail_instance` releases capacity only for
/// entries it actually removes. A guard that fires after `sweep_leases`
/// already tore this handle down therefore finds nothing and changes nothing
/// (proved by `teardown_runs_twice_without_double_releasing`).
struct RegistrationGuard {
    state: Arc<AppState>,
    handle: u64,
    identity: AgentIdentity,
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        // Must not panic: a panic here during an unwind aborts the process.
        // Teardown is lock-guarded map mutation, non-blocking sends, and
        // FAIL-SOFT frame/event serialization. This Drop chain reaches
        // `fail_instance` and `EventHub::emit`; the teardown-adjacent
        // `sweep_deadlines` (called from the lease sweeper task, not from
        // here) shares the same fail-soft discipline. All three drop a frame
        // with an error log instead of panicking on a serialization error
        // (see `synthesized_frame` and `emit`), so no `expect`/`unwrap` lies
        // on this path. parking_lot locks are not poisoned and are released
        // by the unwind itself, so a panic taken while holding one cannot
        // deadlock this call.
        teardown(&self.state, self.handle, &self.identity);
    }
}

/// Announce a fresh registration to the namespace's observers.
fn announce_registration(state: &Arc<AppState>, handle: u64) {
    if let Some(i) = state.registry.get(handle) {
        state.events.emit(
            &state.registry,
            &i.namespace,
            CpEvent::AgentRegistered {
                agent: i.logical_id(),
                agent_type: i.agent_type.clone(),
                instance_id: i.instance_id,
                labels: i.labels,
            },
        );
    }
}

/// Deregister an instance, announce it to the lobby, and fail its in-flight
/// delegations. Shared by socket teardown and lease expiry — the only
/// difference an observer sees is the [`DeregisterReason`].
///
/// Invoked from [`RegistrationGuard::drop`], so it runs on the normal return
/// path and on an unwind alike.
///
/// Deliberately idempotent with the sweeper: when `sweep_leases` already ran
/// this for the handle, the `deregister` finds nothing (so no second
/// announcement is emitted) and `fail_instance` finds no in-flight entries —
/// both calls are no-ops. That idempotency is a contract: `fail_instance`
/// releases capacity only for entries it actually removes, so a second pass
/// can never double-release (see the capacity note in `Router::delegate`'s
/// rollback).
fn deregister_and_announce(state: &Arc<AppState>, handle: u64, reason: DeregisterReason) {
    if let Some(i) = state.registry.deregister(handle) {
        // Emitted after removal: a dying connection is never a fan-out target.
        state.events.emit(
            &state.registry,
            &i.namespace,
            CpEvent::AgentDeregistered {
                agent: i.logical_id(),
                instance_id: i.instance_id,
                reason,
            },
        );
    }
    let mut next = || state.next_rpc_id();
    for (inst, frame) in
        state
            .router
            .fail_instance(&state.registry, &state.events, handle, &mut next)
    {
        let _ = inst.tx.try_send(frame);
    }
}

/// Deregister this connection's own registration (by handle — cannot touch
/// another connection's entry) and fail its in-flight delegations.
fn teardown(state: &Arc<AppState>, handle: u64, identity: &AgentIdentity) {
    deregister_and_announce(state, handle, DeregisterReason::Disconnect);
    info!(
        agent = %format!("{}/{}", identity.namespace, identity.name),
        handle,
        "disconnected"
    );
}

/// Validate the registration frame against the authenticated identity.
/// Returns the parsed params and the request id, or an error payload.
fn parse_register(
    text: &str,
    identity: &AgentIdentity,
) -> Result<(RegisterParams, u64), (u64, ErrorObject)> {
    let msg: JsonRpcMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            return Err((
                0,
                ErrorObject::new(codes::INVALID_PARAMS, format!("malformed frame: {e}")),
            ))
        }
    };
    let rpc_id = match msg.require_request_envelope() {
        Ok(id) => id,
        Err(err) => return Err((msg.id.unwrap_or(0), err)),
    };
    if msg.method.as_deref() != Some(methods::REGISTER) {
        return Err((
            rpc_id,
            ErrorObject::new(codes::NOT_REGISTERED, "first frame must be cp/register"),
        ));
    }
    let params: RegisterParams = match msg.params.and_then(|p| serde_json::from_value(p).ok()) {
        Some(p) => p,
        None => {
            return Err((
                rpc_id,
                ErrorObject::new(codes::INVALID_PARAMS, "invalid cp/register params"),
            ))
        }
    };
    if params.protocol_version != PROTOCOL_VERSION {
        return Err((
            rpc_id,
            ErrorObject::new(
                codes::UNSUPPORTED_VERSION,
                format!(
                    "protocol version {} unsupported (CP speaks {})",
                    params.protocol_version, PROTOCOL_VERSION
                ),
            ),
        ));
    }
    // Identity binding: claims must match the key's bound identity exactly.
    if params.namespace != identity.namespace
        || params.name != identity.name
        || params.agent_type != identity.agent_type
    {
        return Err((
            rpc_id,
            ErrorObject::new(
                codes::IDENTITY_MISMATCH,
                format!(
                    "registration claims {}/{} ({}) do not match the identity bound to this key",
                    params.namespace, params.name, params.agent_type
                ),
            ),
        ));
    }
    if params.instance_id.trim().is_empty() {
        return Err((
            rpc_id,
            ErrorObject::new(codes::INVALID_PARAMS, "instance_id must be non-empty"),
        ));
    }
    Ok((params, rpc_id))
}

/// Dispatch one post-registration frame. Returns an optional direct reply.
fn handle_frame(state: &Arc<AppState>, handle: u64, text: &str) -> Option<String> {
    let msg: JsonRpcMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            let resp = JsonRpcErrorResponse::new(
                0,
                ErrorObject::new(codes::INVALID_PARAMS, format!("malformed frame: {e}")),
            );
            return Some(serde_json::to_string(&resp).expect("serializable"));
        }
    };
    // Responses to CP-issued requests (forwarded delegates, cancels): v1
    // correlates by delegation_id inside result frames, so plain JSON-RPC
    // acks are dropped.
    let method = msg.method.as_deref()?.to_string();
    let rpc_id = match msg.require_request_envelope() {
        Ok(id) => id,
        Err(err) => {
            let resp = JsonRpcErrorResponse::new(msg.id.unwrap_or(0), err);
            return Some(serde_json::to_string(&resp).expect("serializable"));
        }
    };
    // The sender's identity claims are never read from the frame: everything
    // derives from the authenticated registration behind `handle`.
    //
    // The registration can be gone while this task is still running: the
    // sweeper deregisters an expired lease and signals the connection, but the
    // signal is observed asynchronously, so frames already in flight land
    // here first. Answer them instead of dropping them silently — a client
    // whose heartbeat vanished into nothing cannot tell a swept lease from a
    // hung CP, whereas NOT_REGISTERED tells it exactly what to do (reconnect
    // and register again; registration is first-frame-only).
    let me = match state.registry.get(handle) {
        Some(i) => i,
        None => {
            warn!(
                handle,
                method = %method,
                "frame on a connection whose registration is gone (swept lease) — NOT_REGISTERED"
            );
            let resp = JsonRpcErrorResponse::new(
                rpc_id,
                ErrorObject::new(
                    codes::NOT_REGISTERED,
                    "connection is no longer registered (lease expired); reconnect and re-register",
                ),
            );
            return Some(serde_json::to_string(&resp).expect("serializable"));
        }
    };

    // Observers are read-only. Policy already denies their delegations and
    // ownership checks drop their results/cancels; rejecting up front turns a
    // silent drop into an actionable error.
    if me.agent_type == AgentType::Observer
        && (method == methods::DELEGATE
            || method == methods::DELEGATE_RESULT
            || method == methods::CANCEL)
    {
        let resp = JsonRpcErrorResponse::new(
            rpc_id,
            ErrorObject::new(
                codes::POLICY_DENIED,
                format!(
                    "{method} is not available to observers: they are read-only \
                     (cp/heartbeat, cp/list_agents, and cp/event only)"
                ),
            ),
        );
        return Some(serde_json::to_string(&resp).expect("serializable"));
    }

    macro_rules! params_or_err {
        ($ty:ty) => {
            match msg
                .params
                .clone()
                .and_then(|p| serde_json::from_value::<$ty>(p).ok())
            {
                Some(p) => p,
                None => {
                    let resp = JsonRpcErrorResponse::new(
                        rpc_id,
                        ErrorObject::new(codes::INVALID_PARAMS, "invalid params"),
                    );
                    return Some(serde_json::to_string(&resp).expect("serializable"));
                }
            }
        };
    }

    match method.as_str() {
        methods::HEARTBEAT => {
            let _p = params_or_err!(crate::proto::HeartbeatParams);
            state.registry.heartbeat(handle);
            let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
            Some(serde_json::to_string(&resp).expect("serializable"))
        }
        methods::DELEGATE => {
            let p = params_or_err!(DelegateParams);
            if p.prompt.len() > state.cfg.max_prompt_bytes {
                let resp = JsonRpcErrorResponse::new(
                    rpc_id,
                    ErrorObject::new(
                        codes::INVALID_PARAMS,
                        format!(
                            "prompt exceeds max_prompt_bytes ({})",
                            state.cfg.max_prompt_bytes
                        ),
                    ),
                );
                return Some(serde_json::to_string(&resp).expect("serializable"));
            }
            let outcome = state.router.delegate(
                &state.cfg,
                &state.registry,
                &state.events,
                &me.namespace,
                &me.name,
                &me.agent_type,
                handle,
                p,
                state.next_rpc_id(),
            );
            let reply = match outcome {
                DelegateOutcome::Accepted(ack) => serde_json::to_string(&JsonRpcResponse::new(
                    rpc_id,
                    serde_json::to_value(&ack).expect("serializable"),
                )),
                DelegateOutcome::Rejected(err) => {
                    serde_json::to_string(&JsonRpcErrorResponse::new(rpc_id, err))
                }
            };
            Some(reply.expect("serializable"))
        }
        methods::DELEGATE_RESULT => {
            let p = params_or_err!(DelegateResultParams);
            match state.router.complete(
                &state.registry,
                &state.events,
                handle,
                p,
                state.cfg.max_result_bytes,
                state.next_rpc_id(),
            ) {
                CompleteOutcome::Completed {
                    delivered,
                    stalled_initiator,
                } => {
                    if let Some(initiator_handle) = stalled_initiator {
                        // The initiator cannot drain its bounded queue: per
                        // the queue contract it is treated as disconnected,
                        // never silently skipped. The delegation itself
                        // already committed (entry removed, capacity
                        // released, terminal emitted), so the teardown finds
                        // nothing to fail and synthesizes nothing.
                        //
                        // Exception: during the drain the queue was closed on
                        // purpose, not overflowed, and the connection is on its
                        // way out with the shutdown close. Naming
                        // backpressure there would tell a client its queue
                        // overflowed when the real cause is a deploy.
                        if state.shutting_down() {
                            info!(
                                initiator_handle,
                                "delegation terminal undeliverable during shutdown — \
                                 the connection is closing with the shutdown reason"
                            );
                        } else {
                            state
                                .registry
                                .signal_shutdown(initiator_handle, REASON_BACKPRESSURE);
                        }
                    }
                    if !delivered {
                        info!(
                            handle,
                            "delegation completed and committed, but the terminal \
                             frame did not reach the initiator (gone or stalled)"
                        );
                    }
                    // The commit is what the serving side is acked for: its
                    // work is done and the delegation is over. Whether the
                    // initiator's connection survived long enough to receive
                    // the frame is a CP-internal matter, and the ack stays
                    // byte-identical to the dropped case so the reply is
                    // never an oracle for initiator liveness.
                    let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
                // Dropped as unknown/foreign/stale, or a concurrent path
                // (cancel, sweep, disconnect) ended the delegation first and
                // owns its terminals; each case is logged in the router (late
                // results after a CP restart are expected).
                CompleteOutcome::Dropped => {
                    let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
            }
        }
        methods::CANCEL => {
            let mut p = params_or_err!(CancelParams);
            // Defence in depth on initiator free text. The event path already
            // redacts this string in `metadata_only` namespaces and truncates
            // it elsewhere, but capping at the entry keeps an oversized reason
            // from being carried through the router and the forwarded frame at
            // all — the same posture `max_prompt_bytes` takes on the delegate
            // path, one layer earlier than the excerpt cap.
            if p.reason.len() > state.cfg.max_event_excerpt_bytes {
                p.reason = crate::router::truncate_with_marker(
                    &p.reason,
                    state.cfg.max_event_excerpt_bytes,
                );
            }
            match state.router.cancel(
                &state.registry,
                &state.events,
                handle,
                &p,
                state.next_rpc_id(),
            ) {
                Ok(forward) => {
                    if let Some((target, frame)) = forward {
                        let _ = target.tx.try_send(frame);
                    }
                    let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
                Err(err) => Some(
                    serde_json::to_string(&JsonRpcErrorResponse::new(rpc_id, err))
                        .expect("serializable"),
                ),
            }
        }
        // Namespace-scoped roster. Open to any registered client (observers
        // included) — the scope is the caller's authenticated namespace, never
        // a frame-supplied one. v1 takes no params, so an absent or empty
        // params object is equally acceptable.
        methods::LIST_AGENTS => {
            let agents: Vec<AgentSummary> = state
                .registry
                .list(&me.namespace)
                .into_iter()
                .map(|i| AgentSummary {
                    name: i.name,
                    agent_type: i.agent_type,
                    instance_id: i.instance_id,
                    labels: i.labels,
                    active_sessions: i.active_sessions,
                    max_delegated_sessions: i.max_delegated_sessions,
                })
                .collect();
            let result = ListAgentsResult {
                namespace: me.namespace.clone(),
                agents,
            };
            let resp =
                JsonRpcResponse::new(rpc_id, serde_json::to_value(&result).expect("serializable"));
            Some(serde_json::to_string(&resp).expect("serializable"))
        }
        other => {
            let resp = JsonRpcErrorResponse::new(
                rpc_id,
                ErrorObject::new(codes::METHOD_NOT_FOUND, format!("unknown method {other}")),
            );
            Some(serde_json::to_string(&resp).expect("serializable"))
        }
    }
}

/// One lease-expiry pass: drop registrations whose lease elapsed, close their
/// connections, and fail their in-flight delegations.
///
/// Signalling the connection is what makes the deregistration complete:
/// without it the connection task keeps running against
/// a registration that no longer exists — every later frame (heartbeats
/// included) is answered `NOT_REGISTERED` at best, and the client cannot
/// re-register because registration is first-frame-only.
///
/// `lease` is a parameter so tests can sweep with a zero window.
pub fn sweep_leases(state: &Arc<AppState>, lease: Duration) {
    for handle in state.registry.expired(lease) {
        warn!(
            handle,
            "lease expired — deregistering and closing connection"
        );
        // Signal first: `deregister` drops the registry's side of the signal.
        state.registry.signal_shutdown(handle, REASON_LEASE_EXPIRED);
        // Removal, the `lease_expired` announcement, and in-flight failure
        // all live in one place, shared with socket teardown.
        deregister_and_announce(state, handle, DeregisterReason::LeaseExpired);
    }
}

/// Background sweeps: lease expiry and delegation deadlines.
pub async fn run_sweeper(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let lease = Duration::from_secs(state.cfg.lease_expiry_secs);
    loop {
        tick.tick().await;

        sweep_leases(&state, lease);

        // Deadline sweep.
        let mut next = || state.next_rpc_id();
        for (inst, frame) in state.router.sweep_deadlines(
            &state.registry,
            &state.events,
            chrono::Utc::now(),
            &mut next,
        ) {
            let _ = inst.tx.try_send(frame);
        }
    }
}

/// Graceful shutdown (SIGTERM/SIGHUP/SIGINT): resolve every in-flight
/// delegation with synthesized terminal frames, latch the process-wide close
/// signal, then wait — bounded by the SAME `shutdown_drain_secs` deadline the
/// connection tasks are working against — for the connection tasks to flush
/// their queues and exit.
///
/// Runs concurrently with the listener's own graceful shutdown, NOT after it:
/// hyper's wait ends when the last in-flight HTTP request finishes, and a peer
/// that opened a socket and then said nothing would pin that wait for as long
/// as the orchestrator lets the process live — taking the drain down with it.
/// The caller stops the sweeper once this returns; the sweeper deliberately
/// outlives the drain so a mid-drain lease/deadline expiry still synthesizes
/// normally.
///
/// Order is deliberate: terminals are synthesized BEFORE the close signal
/// so they are already on each connection's outbound queue when it starts
/// draining. Signalling first would let tasks close ahead of the frames
/// they were about to deliver, and "synthesize where possible" would
/// degrade to "almost never".
pub async fn graceful_shutdown(state: &Arc<AppState>) {
    info!("draining connections and resolving in-flight delegations");
    let mut next = || state.next_rpc_id();
    let mut undeliverable = 0usize;
    for (inst, frame) in
        state
            .router
            .fail_all(&state.registry, &state.events, REASON_SHUTDOWN, &mut next)
    {
        if inst.tx.try_send(frame).is_err() {
            // The peer's bounded queue is already full (or was closed by a
            // connection leaving the drain): the terminal cannot be delivered
            // and the observer event is already emitted, so the two surfaces
            // disagree — unavoidably, since a full queue is exactly a peer
            // that stopped reading. Named at warn level because the drain is
            // the last chance this delegation could ever be resolved for that
            // initiator, and its only remaining outcome is deadline
            // reconciliation on the client.
            undeliverable += 1;
            warn!(
                agent = %inst.logical_id(),
                handle = inst.handle,
                "shutdown terminal could not be queued — the peer's outbound \
                 queue refused it; the delegation will be reconciled by the \
                 initiator's deadline"
            );
        }
    }
    // The deadline is latched with the close signal below, and covers
    // everything after it.
    let drain = Duration::from_secs(state.cfg.shutdown_drain_secs);
    state.begin_shutdown(REASON_SHUTDOWN);
    let outcome = tokio::time::timeout(drain, state.connections_drained()).await;
    if outcome.is_err() {
        warn!(
            drain_secs = state.cfg.shutdown_drain_secs,
            undeliverable, "shutdown drain budget elapsed — exiting with connections still open"
        );
    } else {
        info!(undeliverable, "all connections drained");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::AgentType;

    fn identity() -> AgentIdentity {
        AgentIdentity {
            key: "k".into(),
            namespace: "prod".into(),
            name: "koudu".into(),
            agent_type: AgentType::Primary,
            max_delegated_sessions_cap: None,
        }
    }

    #[test]
    fn register_valid() {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "cp/register",
            "params": {
                "protocol_version": 1,
                "namespace": "prod",
                "name": "koudu",
                "type": "primary",
                "instance_id": "i-1"
            }
        })
        .to_string();
        let (params, rpc) = parse_register(&frame, &identity()).unwrap();
        assert_eq!(params.instance_id, "i-1");
        assert_eq!(rpc, 1);
    }

    #[test]
    fn register_identity_mismatch_rejected() {
        for (ns, name, ty) in [
            ("dev", "koudu", "primary"),
            ("prod", "other", "primary"),
            ("prod", "koudu", "worker"),
        ] {
            let frame = serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "cp/register",
                "params": {
                    "protocol_version": 1,
                    "namespace": ns,
                    "name": name,
                    "type": ty,
                    "instance_id": "i-1"
                }
            })
            .to_string();
            let (_, err) = parse_register(&frame, &identity()).unwrap_err();
            assert_eq!(err.code, codes::IDENTITY_MISMATCH, "{ns}/{name}/{ty}");
        }
    }

    #[test]
    fn register_wrong_first_method_rejected() {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "cp/heartbeat", "params": {"instance_id": "i-1"}
        })
        .to_string();
        let (_, err) = parse_register(&frame, &identity()).unwrap_err();
        assert_eq!(err.code, codes::NOT_REGISTERED);
    }

    #[test]
    fn register_unsupported_version_rejected() {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "cp/register",
            "params": {
                "protocol_version": 99,
                "namespace": "prod",
                "name": "koudu",
                "type": "primary",
                "instance_id": "i-1"
            }
        })
        .to_string();
        let (_, err) = parse_register(&frame, &identity()).unwrap_err();
        assert_eq!(err.code, codes::UNSUPPORTED_VERSION);
    }

    #[test]
    fn register_empty_instance_id_rejected() {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "cp/register",
            "params": {
                "protocol_version": 1,
                "namespace": "prod",
                "name": "koudu",
                "type": "primary",
                "instance_id": "  "
            }
        })
        .to_string();
        let (_, err) = parse_register(&frame, &identity()).unwrap_err();
        assert_eq!(err.code, codes::INVALID_PARAMS);
    }

    #[test]
    fn register_invalid_envelope_rejected() {
        // Missing jsonrpc field: the envelope is validated, not assumed.
        let no_ver = serde_json::json!({
            "id": 6, "method": "cp/register",
            "params": {
                "protocol_version": 1,
                "namespace": "prod",
                "name": "koudu",
                "type": "primary",
                "instance_id": "i-1"
            }
        })
        .to_string();
        let (_, err) = parse_register(&no_ver, &identity()).unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);

        // Notification shape: no id.
        let no_id = serde_json::json!({
            "jsonrpc": "2.0", "method": "cp/register",
            "params": {
                "protocol_version": 1,
                "namespace": "prod",
                "name": "koudu",
                "type": "primary",
                "instance_id": "i-1"
            }
        })
        .to_string();
        let (_, err) = parse_register(&no_id, &identity()).unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    fn state_with(cfg_toml: &str) -> Arc<AppState> {
        let cfg: CpConfig = toml::from_str(cfg_toml).unwrap();
        cfg.validate().unwrap();
        Arc::new(AppState::new(cfg))
    }

    #[test]
    fn conn_quota_bounds_and_recycles_slots() {
        // The quota is a hard bound and the guard
        // releases the slot on drop, so no exit path can leak it.
        let state = state_with("max_connections_per_identity = 2");
        let id = identity();
        let p1 = state.try_acquire_conn(&id).expect("slot 1");
        let p2 = state.try_acquire_conn(&id).expect("slot 2");
        assert_eq!(state.conn_count("prod/koudu"), 2);
        assert!(
            state.try_acquire_conn(&id).is_none(),
            "third concurrent connection must be refused"
        );

        drop(p1);
        assert_eq!(state.conn_count("prod/koudu"), 1);
        let p3 = state
            .try_acquire_conn(&id)
            .expect("released slot is reusable");
        drop(p2);
        drop(p3);
        assert_eq!(state.conn_count("prod/koudu"), 0);
        assert!(state.try_acquire_conn(&id).is_some());
    }

    #[test]
    fn conn_quota_is_per_identity() {
        let state = state_with("max_connections_per_identity = 1");
        let a = identity();
        let mut b = identity();
        b.key = "k2".into();
        b.name = "worker-1".into();
        let _pa = state.try_acquire_conn(&a).expect("koudu slot");
        let _pb = state
            .try_acquire_conn(&b)
            .expect("worker-1 has its own quota");
        assert!(
            state.try_acquire_conn(&a).is_none(),
            "quota is per identity, not global"
        );
        assert_eq!(state.conn_count("prod/koudu"), 1);
        assert_eq!(state.conn_count("prod/worker-1"), 1);
    }

    /// Register one instance on `state` with a fresh outbound queue.
    fn register_test_instance(
        state: &Arc<AppState>,
        name: &str,
        agent_type: AgentType,
        max_sessions: u32,
    ) -> (u64, crate::registry::FrameRx) {
        let (tx, rx) = outbound_channel(1024 * 1024);
        let handle = state.registry.register_conn(
            Instance {
                handle: 0,
                namespace: "prod".into(),
                name: name.into(),
                agent_type,
                instance_id: format!("i-{name}"),
                labels: Default::default(),
                max_delegated_sessions: max_sessions,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx,
            },
            crate::registry::shutdown_signal(),
        );
        (handle, rx)
    }

    /// Route one delegation through the wire-facing handler and return the
    /// admission token from the ack.
    fn delegate_through_handler(state: &Arc<AppState>, from: u64, id: &str, target: &str) -> u64 {
        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "cp/delegate",
            "params": {
                "delegation_id": id,
                "target": {"name": target},
                "prompt": "do it",
                "deadline": deadline
            }
        })
        .to_string();
        let ack: serde_json::Value =
            serde_json::from_str(&handle_frame(state, from, &frame).expect("answered")).unwrap();
        assert!(
            ack.get("error").is_none(),
            "delegation must be accepted: {ack}"
        );
        ack["result"]["admission"]
            .as_u64()
            .expect("token on the ack")
    }

    #[tokio::test]
    async fn shutdown_survives_an_initiator_whose_queue_is_already_full() {
        // The one outcome the drain cannot repair: an initiator whose bounded
        // queue is full when the terminal is synthesized. The frame cannot be
        // delivered — that is what "bounded" means — so the two surfaces
        // disagree by construction: the observer event is emitted, the wire
        // frame is refused. What must NOT happen is a panic, a delegation row
        // left behind, a leaked serving reservation, or a drain that never
        // ends. The refusal is logged at warn level, because the drain is the
        // last chance that delegation had to be resolved for that initiator.
        let state = state_with("");
        let (h_i, mut rx_i) = register_test_instance(&state, "koudu", AgentType::Primary, 4);
        let (h_w, mut rx_w) = register_test_instance(&state, "worker-1", AgentType::Worker, 1);
        let (_h_o, mut rx_o) = register_test_instance(&state, "lobby", AgentType::Observer, 0);
        delegate_through_handler(&state, h_i, "d-1", "worker-1");
        rx_w.try_recv().expect("worker received the forward");
        while rx_o.try_recv().is_ok() {}

        // Nobody drains this queue — exactly the situation a peer that stopped
        // reading creates, and the only way to make `try_send` refuse.
        let filler = "x".repeat(64 * 1024);
        let mut enqueued = 0;
        while state
            .registry
            .get(h_i)
            .unwrap()
            .tx
            .try_send(filler.clone())
            .is_ok()
        {
            enqueued += 1;
            assert!(enqueued < 1000, "the bounded queue must refuse eventually");
        }
        assert!(enqueued > 0, "the first frame must fit");

        graceful_shutdown(&state).await;

        assert_eq!(
            state.router.inflight_count(),
            0,
            "the delegation must still be ended"
        );
        assert_eq!(
            state.registry.get(h_w).unwrap().active_sessions,
            0,
            "the serving reservation must still be released"
        );
        assert!(
            state.shutting_down(),
            "the CP must stay latched as draining afterwards"
        );
        let mut terminal_queued = false;
        while let Ok(frame) = rx_i.try_recv() {
            if frame.contains("cp/delegate_result") {
                terminal_queued = true;
            }
        }
        assert!(
            !terminal_queued,
            "a full queue cannot take the terminal — the documented exception"
        );
        let events: Vec<String> = std::iter::from_fn(|| rx_o.try_recv().ok()).collect();
        assert!(
            events.iter().any(|e| e.contains("delegation_completed")),
            "the observer-side terminal is still emitted: that is why the two \
             surfaces disagree, and why the refusal must be logged: {events:?}"
        );
        assert!(
            rx_w.try_recv().unwrap().contains("cp/cancel"),
            "the serving runtime's queue was healthy, so its cancel must land"
        );
    }

    #[test]
    fn registered_teardown_survives_a_panic() {
        // Teardown used to run only on the connection task's normal return
        // path. A panic after successful registration — reachable from the
        // `expect("serializable")` sites on production paths — skipped it: the
        // RAII `ConnPermit` still freed the identity's quota, but the registry
        // entry and the in-flight rows survived until the lease swept them,
        // keeping capacity reserved on OTHER instances for up to
        // `lease_expiry_secs`. The Drop guard makes the unwind path identical
        // to the normal one.
        let state = state_with("");
        let (h_i, _rx_i) = register_test_instance(&state, "koudu", AgentType::Primary, 4);
        let (h_w, mut rx_w) = register_test_instance(&state, "worker-1", AgentType::Worker, 1);
        // An observer is attached so the unwind exercises the FULL teardown
        // emit surface — the deregister announcement and the per-admission
        // terminal — which must be fail-soft: this Drop may already be
        // unwinding, where a second panic aborts the process (review
        // round-10 F62).
        let (h_o, mut rx_o) = register_test_instance(&state, "lobby", AgentType::Observer, 0);
        let _ = h_o;
        delegate_through_handler(&state, h_i, "d-1", "worker-1");
        rx_w.try_recv().expect("worker received the forward");
        // Drain the events the delegation produced so far.
        while rx_o.try_recv().is_ok() {}
        assert_eq!(state.registry.get(h_w).unwrap().active_sessions, 1);
        assert_eq!(state.router.inflight_count(), 1);

        // Panic inside the registered lifetime of the INITIATOR's connection.
        let silent = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _registered = RegistrationGuard {
                state: Arc::clone(&state),
                handle: h_i,
                identity: identity(),
            };
            panic!("expect(\"serializable\") on a production path");
        }));
        std::panic::set_hook(silent);
        assert!(panicked.is_err(), "the test must actually unwind");

        // Everything the guard owns is reclaimed immediately — not at lease
        // expiry.
        assert!(
            state.registry.get(h_i).is_none(),
            "the registry entry must be gone"
        );
        assert_eq!(
            state.router.inflight_count(),
            0,
            "in-flight rows must be reclaimed"
        );
        assert_eq!(
            state.registry.get(h_w).unwrap().active_sessions,
            0,
            "capacity reserved on the serving instance must be released"
        );
        // ...and the serving runtime is told to stop working.
        let cancel = rx_w.try_recv().expect("downstream cancel was queued");
        assert!(cancel.contains("cp/cancel") && cancel.contains("d-1"));
        // The observer received the teardown's whole emit surface — produced
        // during the unwind without a second panic: the deregister
        // announcement and the delegation's terminal.
        let mut saw_deregistered = false;
        let mut saw_cancelled = false;
        while let Ok(f) = rx_o.try_recv() {
            saw_deregistered |= f.contains("agent_deregistered");
            saw_cancelled |= f.contains("delegation_cancelled");
        }
        assert!(saw_deregistered, "deregister announcement emitted in Drop");
        assert!(saw_cancelled, "delegation terminal emitted in Drop");
    }

    #[test]
    fn teardown_runs_twice_without_double_releasing() {
        // The guard can fire on a handle that was already torn down — the lease
        // sweeper runs the same `deregister` + `fail_instance` pair on its own
        // initiative. A second pass must change nothing: `deregister` is keyed
        // by handle and returns `None` for an absent entry, and `fail_instance`
        // releases capacity only for entries it actually removes. A double
        // release would be silent (session counts saturate) and would let
        // `saturated()` admit work to a full instance.
        let state = state_with("");
        let (h_i, _rx_i) = register_test_instance(&state, "koudu", AgentType::Primary, 4);
        let (h_w, mut rx_w) = register_test_instance(&state, "worker-1", AgentType::Worker, 4);
        // A second initiator keeps one delegation alive on the same worker, so
        // an over-release shows up as a wrong count rather than a clamp at zero.
        let (h_i2, _rx_i2) = register_test_instance(&state, "koudu-2", AgentType::Primary, 4);
        delegate_through_handler(&state, h_i, "d-1", "worker-1");
        delegate_through_handler(&state, h_i2, "d-2", "worker-1");
        assert_eq!(state.registry.get(h_w).unwrap().active_sessions, 2);
        while rx_w.try_recv().is_ok() {}

        let guard = || RegistrationGuard {
            state: Arc::clone(&state),
            handle: h_i,
            identity: identity(),
        };

        // First pass.
        drop(guard());
        assert!(state.registry.get(h_i).is_none());
        assert_eq!(state.router.inflight_count(), 1, "only d-1 was failed");
        assert_eq!(
            state.registry.get(h_w).unwrap().active_sessions,
            1,
            "exactly d-1's reservation was released"
        );

        // Second pass on the same handle: a no-op.
        drop(guard());
        assert!(state.registry.get(h_i).is_none());
        assert_eq!(state.router.inflight_count(), 1);
        assert_eq!(
            state.registry.get(h_w).unwrap().active_sessions,
            1,
            "a repeated teardown must not release capacity a second time"
        );
        // And the sweeper's own pass over the same handle is equally inert.
        sweep_leases(&state, Duration::ZERO);
        assert_eq!(
            state.router.inflight_count(),
            0,
            "the sweeper expired the remaining leases"
        );
        drop(guard());
        assert_eq!(state.router.inflight_count(), 0);
    }

    #[tokio::test]
    async fn sweep_leases_signals_the_connection_before_dropping_it() {
        // At the sweeper level: the shutdown signal is
        // delivered, not just the registry entry removed. (The end-to-end
        // proof over a real socket lives in tests/ws_lifecycle.rs.)
        let state = state_with("heartbeat_interval_secs = 1\nlease_expiry_secs = 2");
        let signal = crate::registry::shutdown_signal();
        let mut observer = signal.subscribe();
        let (tx, _rx) = outbound_channel(1024 * 1024);
        let handle = state.registry.register_conn(
            Instance {
                handle: 0,
                namespace: "prod".into(),
                name: "koudu".into(),
                agent_type: AgentType::Primary,
                instance_id: "i-1".into(),
                labels: Default::default(),
                max_delegated_sessions: 1,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx,
            },
            Arc::clone(&signal),
        );

        // A live lease is left alone.
        sweep_leases(&state, Duration::from_secs(60));
        assert!(state.registry.get(handle).is_some());
        assert!(observer.borrow().is_none());

        sweep_leases(&state, Duration::ZERO);
        assert!(state.registry.get(handle).is_none());
        observer.changed().await.unwrap();
        assert_eq!(
            *observer.borrow(),
            Some(REASON_LEASE_EXPIRED),
            "the owning connection must be told to close, and why"
        );
    }

    #[tokio::test]
    async fn frame_on_a_swept_handle_is_answered_not_registered() {
        // Frames can arrive between the sweeper dropping a registration and
        // the connection task observing the close signal. They must be
        // answered: silence is indistinguishable from a hung CP, and the
        // client needs to know it has to reconnect and register again.
        let state = state_with("heartbeat_interval_secs = 1\nlease_expiry_secs = 2");
        let signal = crate::registry::shutdown_signal();
        let (tx, _rx) = outbound_channel(1024 * 1024);
        let handle = state.registry.register_conn(
            Instance {
                handle: 0,
                namespace: "prod".into(),
                name: "koudu".into(),
                agent_type: AgentType::Primary,
                instance_id: "i-1".into(),
                labels: Default::default(),
                max_delegated_sessions: 1,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx,
            },
            Arc::clone(&signal),
        );

        // While registered, a heartbeat is answered normally.
        let hb = serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "cp/heartbeat",
            "params": {"instance_id": "i-1"}
        })
        .to_string();
        let ok: serde_json::Value =
            serde_json::from_str(&handle_frame(&state, handle, &hb).expect("answered")).unwrap();
        assert_eq!(ok["result"]["ok"], true);

        // Sweep the lease, then replay the same frame on the same handle.
        sweep_leases(&state, Duration::ZERO);
        assert!(state.registry.get(handle).is_none(), "handle was swept");

        let reply = handle_frame(&state, handle, &hb)
            .expect("a frame on a swept handle must be answered, not dropped");
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["id"], 7, "the error must correlate with the request");
        assert_eq!(v["error"]["code"], codes::NOT_REGISTERED);

        // Same for a delegate attempt: no method reaches the router with an
        // absent registration.
        let del = serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "cp/delegate",
            "params": {
                "delegation_id": "d-1",
                "target": {"name": "worker-1"},
                "prompt": "do it",
                "deadline": "2999-01-01T00:00:00Z"
            }
        })
        .to_string();
        let v2: serde_json::Value =
            serde_json::from_str(&handle_frame(&state, handle, &del).expect("answered")).unwrap();
        assert_eq!(v2["error"]["code"], codes::NOT_REGISTERED);
        assert_eq!(state.router.inflight_count(), 0);
    }

    #[tokio::test]
    async fn stalled_initiator_is_disconnected_and_result_still_commits() {
        // The bounded-queue contract for the terminal result under
        // commit-first: the commit ends the delegation before delivery, so
        // the serving side is acked for the commit (`ok: true`, its work is
        // done), the stalled initiator is closed (treated as disconnected)
        // rather than silently skipped, and its teardown finds nothing —
        // capacity was already released exactly once by the commit.
        let state = state_with("");

        // Initiator whose outbound byte budget one filler frame exhausts —
        // the queue refuses on bytes here, which is the same
        // "cannot drain → disconnected" contract as an exhausted entry count.
        const FILLER: &str = "filler";
        let signal_i = crate::registry::shutdown_signal();
        let mut observer = signal_i.subscribe();
        let (tx_i, _rx_i) = outbound_channel(FILLER.len());
        let h_i = state.registry.register_conn(
            Instance {
                handle: 0,
                namespace: "prod".into(),
                name: "koudu".into(),
                agent_type: AgentType::Primary,
                instance_id: "i-1".into(),
                labels: Default::default(),
                max_delegated_sessions: 4,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx: tx_i.clone(),
            },
            Arc::clone(&signal_i),
        );
        let (tx_w, mut rx_w) = outbound_channel(1024 * 1024);
        let h_w = state.registry.register_conn(
            Instance {
                handle: 0,
                namespace: "prod".into(),
                name: "worker-1".into(),
                agent_type: AgentType::Worker,
                instance_id: "i-2".into(),
                labels: Default::default(),
                max_delegated_sessions: 1,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx: tx_w,
            },
            crate::registry::shutdown_signal(),
        );

        // Route one delegation through the wire-facing handler.
        let deadline = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
        let del = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "cp/delegate",
            "params": {
                "delegation_id": "d-1",
                "target": {"name": "worker-1"},
                "prompt": "do it",
                "deadline": deadline
            }
        })
        .to_string();
        let ack: serde_json::Value =
            serde_json::from_str(&handle_frame(&state, h_i, &del).expect("answered")).unwrap();
        assert!(ack.get("error").is_none(), "delegation must be accepted");
        let admission = ack["result"]["admission"]
            .as_u64()
            .expect("the ack must carry the admission token");
        rx_w.try_recv().expect("worker received the forward frame");

        // Fill the initiator's queue to its byte budget, then complete.
        tx_i.try_send(FILLER.into()).unwrap();
        let res = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "cp/delegate_result",
            "params": {
                "delegation_id": "d-1",
                "admission": admission,
                "status": "completed",
                "result": "done"
            }
        })
        .to_string();
        let reply: serde_json::Value =
            serde_json::from_str(&handle_frame(&state, h_w, &res).expect("answered")).unwrap();
        assert!(
            reply.get("error").is_none(),
            "the commit ended the delegation; the serving side is acked for it"
        );
        assert_eq!(reply["result"]["ok"], true);
        assert_eq!(reply["id"], 2, "the ack must correlate with the request");

        // The initiator is told to close, with the backpressure reason.
        observer.changed().await.unwrap();
        assert_eq!(*observer.borrow(), Some(REASON_BACKPRESSURE));

        // The commit already ended the delegation and released capacity:
        // teardown of the stalled initiator finds nothing to fail, so no
        // second terminal and no double release can occur.
        assert_eq!(state.router.inflight_count(), 0);
        assert_eq!(state.registry.get(h_w).unwrap().active_sessions, 0);
        let mut next = || 9;
        let frames = state
            .router
            .fail_instance(&state.registry, &state.events, h_i, &mut next);
        assert!(frames.is_empty());
        assert_eq!(state.registry.get(h_w).unwrap().active_sessions, 0);
    }

    // --- observer / lobby wiring (Phase 1) ---

    fn join(
        state: &Arc<AppState>,
        ns: &str,
        name: &str,
        ty: AgentType,
    ) -> (u64, crate::registry::FrameRx) {
        join_at(state, ns, name, ty, Instant::now())
    }

    fn join_at(
        state: &Arc<AppState>,
        ns: &str,
        name: &str,
        ty: AgentType,
        last_heartbeat: Instant,
    ) -> (u64, crate::registry::FrameRx) {
        let (tx, rx) = crate::registry::outbound_channel(64 * 1024 * 1024);
        let handle = state.registry.register(Instance {
            handle: 0,
            namespace: ns.into(),
            name: name.into(),
            agent_type: ty,
            instance_id: format!("i-{name}"),
            labels: Default::default(),
            max_delegated_sessions: 2,
            active_sessions: 0,
            registered_at: Instant::now(),
            last_heartbeat,
            tx,
        });
        (handle, rx)
    }

    fn events_of(rx: &mut crate::registry::FrameRx) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(text) = rx.try_recv() {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["method"], "cp/event");
            out.push(v["params"].clone());
        }
        out
    }

    fn call(state: &Arc<AppState>, handle: u64, method: &str, params: serde_json::Value) -> String {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 42, "method": method, "params": params
        })
        .to_string();
        handle_frame(state, handle, &frame).expect("a reply")
    }

    #[test]
    fn registration_is_announced_to_observers_including_other_observers() {
        let state = state_with("");
        let (_, mut lobby) = join(&state, "prod", "lobby", AgentType::Observer);
        let (h_lobby2, mut lobby2) = join(&state, "prod", "lobby-2", AgentType::Observer);
        let (h_worker, _w_rx) = join(&state, "prod", "worker-1", AgentType::Worker);

        // An observer's own arrival is visible to the lobby (itself included).
        announce_registration(&state, h_lobby2);
        announce_registration(&state, h_worker);

        let seen = events_of(&mut lobby);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0]["event"], "agent_registered");
        assert_eq!(seen[0]["agent"], "prod/lobby-2");
        assert_eq!(seen[0]["type"], "observer");
        assert_eq!(seen[0]["seq"], 1);
        assert_eq!(seen[1]["agent"], "prod/worker-1");
        assert_eq!(seen[1]["type"], "worker");
        assert_eq!(seen[1]["instance_id"], "i-worker-1");
        assert_eq!(seen[1]["seq"], 2);
        assert_eq!(events_of(&mut lobby2).len(), 2);
    }

    #[test]
    fn disconnect_and_lease_expiry_announce_distinct_reasons() {
        let state = state_with("");
        let (_, mut lobby) = join(&state, "prod", "lobby", AgentType::Observer);
        let (h_a, _rx_a) = join(&state, "prod", "worker-a", AgentType::Worker);
        // worker-b stopped heartbeating five minutes ago; the lobby and
        // worker-a are current, so only worker-b's lease is overdue.
        let (h_b, _rx_b) = join_at(
            &state,
            "prod",
            "worker-b",
            AgentType::Worker,
            Instant::now() - std::time::Duration::from_secs(300),
        );

        teardown(&state, h_a, &identity());
        sweep_leases(&state, std::time::Duration::from_secs(60));

        let seen = events_of(&mut lobby);
        assert_eq!(seen.len(), 2, "one disconnect + one lease expiry: {seen:?}");
        assert_eq!(seen[0]["event"], "agent_deregistered");
        assert_eq!(seen[0]["agent"], "prod/worker-a");
        assert_eq!(seen[0]["reason"], "disconnect");
        assert_eq!(seen[0]["seq"], 1);
        assert_eq!(seen[1]["agent"], "prod/worker-b");
        assert_eq!(seen[1]["reason"], "lease_expired");
        assert_eq!(seen[1]["instance_id"], "i-worker-b");
        assert_eq!(seen[1]["seq"], 2);
        assert!(state.registry.get(h_a).is_none());
        assert!(state.registry.get(h_b).is_none());
        assert_eq!(
            state.registry.observers("prod").len(),
            1,
            "the current observer keeps its registration"
        );
    }

    #[test]
    fn list_agents_returns_the_callers_namespace_roster() {
        let state = state_with("");
        let (h_primary, _p) = join(&state, "prod", "koudu", AgentType::Primary);
        let (h_lobby, _l) = join(&state, "prod", "lobby", AgentType::Observer);
        join(&state, "dev", "other", AgentType::Worker);

        for handle in [h_primary, h_lobby] {
            let reply = call(&state, handle, methods::LIST_AGENTS, serde_json::json!({}));
            let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(v["id"], 42);
            assert_eq!(v["result"]["namespace"], "prod");
            let agents = v["result"]["agents"].as_array().unwrap();
            assert_eq!(agents.len(), 2, "dev/other must not leak: {agents:?}");
            let names: Vec<&str> = agents.iter().map(|a| a["name"].as_str().unwrap()).collect();
            assert!(names.contains(&"koudu") && names.contains(&"lobby"));
            let lobby = agents.iter().find(|a| a["name"] == "lobby").unwrap();
            assert_eq!(lobby["type"], "observer");
            assert_eq!(lobby["instance_id"], "i-lobby");
            assert_eq!(lobby["active_sessions"], 0);
            assert_eq!(lobby["max_delegated_sessions"], 2);
        }

        // v1 takes no params: an absent params object is accepted too.
        let frame =
            serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "cp/list_agents"}).to_string();
        let reply = handle_frame(&state, h_primary, &frame).unwrap();
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["namespace"], "prod");
    }

    #[test]
    fn observers_are_rejected_from_the_delegation_methods() {
        let state = state_with("");
        let (h_lobby, _l) = join(&state, "prod", "lobby", AgentType::Observer);
        let (h_worker, _w) = join(&state, "prod", "worker-1", AgentType::Worker);

        for (method, params) in [
            (
                methods::DELEGATE,
                serde_json::json!({
                    "delegation_id": "d-1",
                    "target": {"name": "worker-1"},
                    "prompt": "do it",
                    "deadline": (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339()
                }),
            ),
            (
                methods::DELEGATE_RESULT,
                serde_json::json!({"delegation_id": "d-1", "status": "completed"}),
            ),
            (
                methods::CANCEL,
                serde_json::json!({"delegation_id": "d-1", "reason": "no"}),
            ),
        ] {
            let reply = call(&state, h_lobby, method, params);
            let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(
                v["error"]["code"],
                codes::POLICY_DENIED,
                "{method} must be denied for observers: {v}"
            );
            assert!(v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("read-only"));
        }

        // Heartbeat and list_agents remain available to observers.
        let hb = call(
            &state,
            h_lobby,
            methods::HEARTBEAT,
            serde_json::json!({"instance_id": "i-lobby"}),
        );
        assert!(hb.contains("\"ok\":true"));
        // A non-observer is unaffected by the guard.
        let reply = call(
            &state,
            h_worker,
            methods::CANCEL,
            serde_json::json!({"delegation_id": "d-nope", "admission": 1, "reason": "x"}),
        );
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        // The router's own refusal for an unknown id is a byte-identical
        // POLICY_DENIED (review round-3 F3), so the code alone cannot tell
        // the two denials apart — the message can: only the guard says
        // "read-only".
        assert_eq!(v["error"]["code"], codes::POLICY_DENIED, "{v}");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(
            !msg.contains("read-only") && msg.contains("not in flight for this instance"),
            "worker reaches the router, not the observer guard: {msg}"
        );
    }

    #[test]
    fn delegate_through_handle_frame_emits_lobby_events() {
        let state = state_with("");
        let (h_primary, _p) = join(&state, "prod", "koudu", AgentType::Primary);
        let (h_worker, mut w_rx) = join(&state, "prod", "worker-1", AgentType::Worker);
        let (_, mut lobby) = join(&state, "prod", "lobby", AgentType::Observer);

        let reply = call(
            &state,
            h_primary,
            methods::DELEGATE,
            serde_json::json!({
                "delegation_id": "d-1",
                "target": {"name": "worker-1"},
                "prompt": "ship it",
                "deadline": (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339()
            }),
        );
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["result"]["assigned_to"], "prod/worker-1", "{v}");
        let admission = v["result"]["admission"]
            .as_u64()
            .expect("ack carries the token");
        let forwarded = w_rx.try_recv().unwrap();
        assert!(forwarded.contains("cp/delegate"));

        let reply = call(
            &state,
            h_worker,
            methods::DELEGATE_RESULT,
            serde_json::json!({
                "delegation_id": "d-1",
                "admission": admission,
                "status": "completed",
                "result": "ok"
            }),
        );
        assert!(reply.contains("\"ok\":true"));

        let seen = events_of(&mut lobby);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0]["event"], "delegation_requested");
        assert_eq!(seen[0]["prompt_excerpt"], "ship it");
        assert_eq!(seen[1]["event"], "delegation_completed");
        assert_eq!(seen[1]["result_excerpt"], "ok");
        assert_eq!(seen[0]["seq"], 1);
        assert_eq!(seen[1]["seq"], 2);
    }

    #[tokio::test]
    async fn an_oversized_cancel_reason_is_capped_at_the_entry() {
        // Initiator free text is capped before the router or the forwarded
        // frame ever sees it. The event path redacts/truncates too, but this
        // keeps a multi-megabyte reason from riding through the CP at all.
        let state = state_with("max_event_excerpt_bytes = 256");
        let (h_primary, _p_rx) = join(&state, "prod", "koudu", AgentType::Primary);
        let (_h_worker, mut w_rx) = join(&state, "prod", "worker-1", AgentType::Worker);

        let ack = call(
            &state,
            h_primary,
            methods::DELEGATE,
            serde_json::json!({
                "delegation_id": "d-cap",
                "target": {"name": "worker-1"},
                "prompt": "work",
                "deadline": (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339()
            }),
        );
        let v: serde_json::Value = serde_json::from_str(&ack).unwrap();
        let admission = v["result"]["admission"]
            .as_u64()
            .expect("ack carries the token");
        w_rx.try_recv().expect("forwarded");

        let huge = "A".repeat(64 * 1024);
        let reply = call(
            &state,
            h_primary,
            methods::CANCEL,
            serde_json::json!({
                "delegation_id": "d-cap",
                "admission": admission,
                "reason": huge
            }),
        );
        assert!(
            reply.contains("\"ok\":true"),
            "the cancel itself succeeds: {reply}"
        );

        // The forwarded cp/cancel carries the capped reason, not 64 KiB.
        let forwarded = w_rx.try_recv().expect("cancel forwarded to the worker");
        assert!(
            forwarded.len() < 2048,
            "the forwarded cancel must not carry the untruncated reason ({} bytes)",
            forwarded.len()
        );
        assert!(
            forwarded.contains("truncated by control plane"),
            "the cap leaves its marker: {forwarded}"
        );
    }
}

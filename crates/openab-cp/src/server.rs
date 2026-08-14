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
use crate::proto::{
    codes, methods, CancelParams, DelegateParams, DelegateResultParams, ErrorObject,
    JsonRpcErrorResponse, JsonRpcMessage, JsonRpcResponse, RegisterAck, RegisterParams,
    PROTOCOL_VERSION,
};
use crate::registry::{outbound_channel, shutdown_signal, Instance, Registry};
use crate::router::{CompleteOutcome, DelegateOutcome, Router};

pub struct AppState {
    pub cfg: CpConfig,
    pub registry: Registry,
    pub router: Router,
    rpc_id: AtomicU64,
    /// Live connections per identity (`namespace/name`), counted from the
    /// upgrade so pre-registration sockets are bounded too.
    conns: Mutex<BTreeMap<String, u32>>,
}

impl AppState {
    pub fn new(cfg: CpConfig) -> Self {
        Self {
            cfg,
            registry: Registry::new(),
            router: Router::new(),
            rpc_id: AtomicU64::new(1),
            conns: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn next_rpc_id(&self) -> u64 {
        self.rpc_id.fetch_add(1, Ordering::Relaxed)
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
async fn send_bounded(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: Message,
    write_timeout: Duration,
    shutdown_rx: &mut watch::Receiver<Option<&'static str>>,
) -> Result<(), WriteStop> {
    tokio::select! {
        _ = shutdown_rx.changed() => Err(WriteStop::Shutdown(*shutdown_rx.borrow_and_update())),
        sent = tokio::time::timeout(write_timeout, sink.send(msg)) => match sent {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(WriteStop::Disconnected),
            Err(_) => Err(WriteStop::Disconnected),
        },
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

    // Shutdown signal so the CP can close this socket when it drops the
    // registration on its own initiative (lease expiry) or must terminate the
    // connection (terminal-frame backpressure).
    //
    // Created BEFORE the registration read — and therefore before the registry
    // ever holds a clone — so no signal can be missed, and every write in this
    // task, registration-phase writes included, can be raced against it. Kept
    // alive here for the whole connection: closing is driven by an explicit
    // signal, never by the registry happening to drop its side.
    let shutdown = shutdown_signal();
    let mut shutdown_rx = shutdown.subscribe();

    // --- Registration: mandatory first frame, within a deadline ---
    // An authenticated peer must not be able to park idle sockets: pings keep
    // the transport alive but do not extend this deadline.
    let register = match tokio::time::timeout(
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
    )
    .await
    {
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
            let _ = send_bounded(
                &mut sink,
                policy_close(REASON_REGISTER_TIMEOUT),
                write_timeout,
                &mut shutdown_rx,
            )
            .await;
            return;
        }
    };
    let (reg, reg_rpc_id) = match parse_register(&register, &identity) {
        Ok(ok) => ok,
        Err((id, err)) => {
            let resp = JsonRpcErrorResponse::new(id, err);
            let _ = send_bounded(
                &mut sink,
                Message::Text(serde_json::to_string(&resp).expect("serializable").into()),
                write_timeout,
                &mut shutdown_rx,
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
    let handle = state.registry.register_conn(
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
    );
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
        &mut shutdown_rx,
    )
    .await
    {
        // CP-initiated closes carry meaning even here: if the CP signalled
        // this connection while the ack was in flight, still tell the client
        // why before tearing down.
        if let WriteStop::Shutdown(Some(reason)) = stop {
            let _ = send_bounded(
                &mut sink,
                policy_close(reason),
                write_timeout,
                &mut shutdown_rx,
            )
            .await;
        }
        // `_registered` runs teardown on the way out.
        return;
    }

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
            match send_bounded(&mut sink, $msg, write_timeout, &mut shutdown_rx).await {
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
            // The CP dropped this registration (lease expiry) or must
            // terminate the connection (terminal-frame backpressure): the
            // socket must go too. Keeping it open would leave a
            // connection whose every frame hits an absent registry entry and
            // which can never re-register, since registration is
            // first-frame-only. Closing lets the client reconnect,
            // re-authenticate, and register again.
            _ = shutdown_rx.changed() => {
                cp_close_reason = *shutdown_rx.borrow_and_update();
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

    if let Some(reason) = cp_close_reason {
        info!(
            agent = %format!("{}/{}", identity.namespace, identity.name),
            handle,
            reason,
            "closing connection at the CP's request"
        );
        // Bounded like every other write: a peer that has stopped reading must
        // not be able to delay teardown (and its quota slot) by refusing to
        // accept the close frame.
        let _ = send_bounded(
            &mut sink,
            policy_close(reason),
            write_timeout,
            &mut shutdown_rx,
        )
        .await;
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
        // Everything it touches is lock-guarded map mutation and non-blocking
        // sends — no `expect`, no allocation-dependent invariants. parking_lot
        // locks are not poisoned and are released by the unwind itself, so a
        // panic taken while holding one cannot deadlock this call.
        teardown(&self.state, self.handle, &self.identity);
    }
}

/// Deregister this connection's own registration (by handle — cannot touch
/// another connection's entry) and fail its in-flight delegations.
///
/// Invoked from [`RegistrationGuard::drop`], so it runs on the normal return
/// path and on an unwind alike.
///
/// Deliberately idempotent with the sweeper: when `sweep_leases` already ran
/// `deregister` + `fail_instance` for this handle, both calls here find
/// nothing (the registry entry and the in-flight entries are gone) and are
/// no-ops. That idempotency is a contract — `fail_instance` releases
/// capacity only for entries it actually removes, so a second pass can never
/// double-release (see the capacity note in `Router::delegate`'s rollback).
fn teardown(state: &Arc<AppState>, handle: u64, identity: &AgentIdentity) {
    state.registry.deregister(handle);
    let mut next = || state.next_rpc_id();
    for (inst, frame) in state
        .router
        .fail_instance(&state.registry, handle, &mut next)
    {
        let _ = inst.tx.try_send(frame);
    }
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
                handle,
                p,
                state.cfg.max_result_bytes,
                state.next_rpc_id(),
            ) {
                CompleteOutcome::InitiatorStalled { initiator_handle } => {
                    // The initiator cannot drain its bounded queue: per the
                    // queue contract it is treated as disconnected, never
                    // silently skipped. Its teardown fails the delegation
                    // over the fail_instance path (capacity released once,
                    // cp/cancel to this serving runtime). Do NOT ack the
                    // result as delivered — the serving side must know its
                    // result did not reach the initiator.
                    state
                        .registry
                        .signal_shutdown(initiator_handle, REASON_BACKPRESSURE);
                    let resp = JsonRpcErrorResponse::new(
                        rpc_id,
                        ErrorObject::new(
                            codes::TARGET_DISCONNECTED,
                            "initiator cannot receive the result; the delegation will be cancelled",
                        ),
                    );
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
                // The result reached the initiator, which is what the serving
                // side is being acked for. Whether THIS frame also committed
                // the state transition is a CP-internal matter: a concurrent
                // cancel/sweep/disconnect may have ended the delegation
                // first, and the initiator resolves competing terminal frames
                // by "first one wins" (see the ADR wire contract).
                CompleteOutcome::Delivered { committed } => {
                    if !committed {
                        info!(
                            handle,
                            "terminal result delivered, but the delegation had already been \
                             ended (or its id re-admitted) — no state change"
                        );
                    }
                    let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
                // Dropped as unknown/foreign; each case is logged in the
                // router (late results after a CP restart are expected).
                CompleteOutcome::Dropped => {
                    let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
                    Some(serde_json::to_string(&resp).expect("serializable"))
                }
            }
        }
        methods::CANCEL => {
            let p = params_or_err!(CancelParams);
            match state
                .router
                .cancel(&state.registry, handle, &p, state.next_rpc_id())
            {
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
pub fn sweep_leases(state: &Arc<AppState>, lease: Duration) {
    for handle in state.registry.expired(lease) {
        warn!(
            handle,
            "lease expired — deregistering and closing connection"
        );
        // Signal first: `deregister` drops the registry's side of the signal.
        state.registry.signal_shutdown(handle, REASON_LEASE_EXPIRED);
        state.registry.deregister(handle);
        let mut next = || state.next_rpc_id();
        for (inst, frame) in state
            .router
            .fail_instance(&state.registry, handle, &mut next)
        {
            let _ = inst.tx.try_send(frame);
        }
    }
}

/// Background sweeps: lease expiry and delegation deadlines.
pub async fn run_sweeper(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;

        sweep_leases(&state, Duration::from_secs(state.cfg.lease_expiry_secs));

        // Deadline sweep.
        let mut next = || state.next_rpc_id();
        for (inst, frame) in
            state
                .router
                .sweep_deadlines(&state.registry, chrono::Utc::now(), &mut next)
        {
            let _ = inst.tx.try_send(frame);
        }
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
        delegate_through_handler(&state, h_i, "d-1", "worker-1");
        rx_w.try_recv().expect("worker received the forward");
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
    async fn stalled_initiator_is_disconnected_and_serving_side_not_falsely_acked() {
        // The bounded-queue contract for the one frame that matters most:
        // when the initiator's queue refuses the terminal result, the
        // serving side must NOT receive `ok: true`, and the initiator must
        // be closed (treated as disconnected) rather than silently skipped.
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
        assert_eq!(
            reply["error"]["code"],
            codes::TARGET_DISCONNECTED,
            "the serving side must not be acked as delivered"
        );
        assert_eq!(reply["id"], 2, "the error must correlate with the request");

        // The initiator is told to close, with the backpressure reason.
        observer.changed().await.unwrap();
        assert_eq!(*observer.borrow(), Some(REASON_BACKPRESSURE));

        // The delegation is still in flight: teardown of the stalled
        // initiator resolves it through fail_instance (capacity released
        // once, cp/cancel to the serving runtime).
        assert_eq!(state.router.inflight_count(), 1);
        let mut next = || 9;
        let frames = state.router.fail_instance(&state.registry, h_i, &mut next);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].1.contains("cp/cancel"));
        assert_eq!(state.registry.get(h_w).unwrap().active_sessions, 0);
        assert_eq!(state.router.inflight_count(), 0);
    }
}

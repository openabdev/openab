//! WebSocket server: authentication at upgrade, mandatory `cp/register`
//! first frame, then frame dispatch to registry/policy/router.
//!
//! Auth: the runtime presents its key as `Authorization: Bearer <key>` on the
//! upgrade request. Keys never appear in URLs (avoids access-log leakage).
//!
//! Resource bounds (review F5): the WS transport enforces
//! `max_frame_bytes` before parsing; each connection's outbound queue is
//! bounded — a peer that cannot drain it is treated as disconnected.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router as AxumRouter;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::{AgentIdentity, CpConfig};
use crate::proto::{
    codes, methods, CancelParams, DelegateParams, DelegateResultParams, ErrorObject,
    JsonRpcErrorResponse, JsonRpcMessage, JsonRpcResponse, RegisterAck, RegisterParams,
    PROTOCOL_VERSION,
};
use crate::registry::{Instance, Registry, OUTBOUND_QUEUE};
use crate::router::{DelegateOutcome, Router};

pub struct AppState {
    pub cfg: CpConfig,
    pub registry: Registry,
    pub router: Router,
    rpc_id: AtomicU64,
}

impl AppState {
    pub fn new(cfg: CpConfig) -> Self {
        Self {
            cfg,
            registry: Registry::new(),
            router: Router::new(),
            rpc_id: AtomicU64::new(1),
        }
    }

    pub fn next_rpc_id(&self) -> u64 {
        self.rpc_id.fetch_add(1, Ordering::Relaxed)
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
    let max_frame = state.cfg.max_frame_bytes;
    ws.max_message_size(max_frame)
        .max_frame_size(max_frame)
        .on_upgrade(move |socket| handle_connection(state, socket, identity))
}

async fn handle_connection(state: Arc<AppState>, socket: WebSocket, identity: AgentIdentity) {
    let (mut sink, mut stream) = socket.split();

    // --- Registration: mandatory first frame ---
    let register = loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => break text,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => {
                warn!(agent = %identity.name, "connection closed before registration");
                return;
            }
        }
    };
    let (reg, reg_rpc_id) = match parse_register(&register, &identity) {
        Ok(ok) => ok,
        Err((id, err)) => {
            let resp = JsonRpcErrorResponse::new(id, err);
            let _ = sink
                .send(Message::Text(
                    serde_json::to_string(&resp).expect("serializable").into(),
                ))
                .await;
            return;
        }
    };

    // Outbound channel for this connection. Bounded (review F5): a peer that
    // cannot drain OUTBOUND_QUEUE frames is disconnected, not buffered.
    let (tx, mut rx) = mpsc::channel::<String>(OUTBOUND_QUEUE);

    let effective_max = match identity.max_delegated_sessions_cap {
        Some(cap) => reg.max_delegated_sessions.min(cap),
        None => reg.max_delegated_sessions,
    };
    // The registry assigns the CP-generated handle (review F1): ownership
    // and teardown never key on the client-supplied instance_id.
    let handle = state.registry.register(Instance {
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
    });
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
    if sink
        .send(Message::Text(
            serde_json::to_string(&resp).expect("serializable").into(),
        ))
        .await
        .is_err()
    {
        teardown(&state, handle, &identity);
        return;
    }

    // --- Main loop: interleave inbound frames and outbound channel ---
    loop {
        tokio::select! {
            outbound = rx.recv() => {
                match outbound {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            inbound = stream.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(reply) = handle_frame(&state, handle, &text) {
                            if sink.send(Message::Text(reply.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        if sink.send(Message::Pong(p)).await.is_err() {
                            break;
                        }
                    }
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

    teardown(&state, handle, &identity);
}

/// Deregister this connection's own registration (by handle — cannot touch
/// another connection's entry) and fail its in-flight delegations.
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
    let me = state.registry.get(handle)?;

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
            if let Some((initiator, frame)) = state.router.complete(
                &state.registry,
                handle,
                p,
                state.cfg.max_result_bytes,
                state.next_rpc_id(),
            ) {
                let _ = initiator.tx.try_send(frame);
            }
            let resp = JsonRpcResponse::new(rpc_id, serde_json::json!({"ok": true}));
            Some(serde_json::to_string(&resp).expect("serializable"))
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

/// Background sweeps: lease expiry and delegation deadlines.
pub async fn run_sweeper(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;

        // Lease expiry → deregister + fail in-flight.
        let lease = std::time::Duration::from_secs(state.cfg.lease_expiry_secs);
        for handle in state.registry.expired(lease) {
            warn!(handle, "lease expired — deregistering");
            state.registry.deregister(handle);
            let mut next = || state.next_rpc_id();
            for (inst, frame) in state
                .router
                .fail_instance(&state.registry, handle, &mut next)
            {
                let _ = inst.tx.try_send(frame);
            }
        }

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
        // Missing jsonrpc field (review F4).
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
}

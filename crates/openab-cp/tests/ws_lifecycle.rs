//! End-to-end WebSocket lifecycle tests: connection termination on lease
//! expiry, pre-registration admission bounds, and the meaning the CP attaches
//! to its own closes (WS 1008 + reason, HTTP 503 naming the quota).
//!
//! These drive a real CP over a loopback socket with a real WS client, which
//! is the only way to prove that the connection *task* reacts — the earlier
//! bug was invisible to unit tests of the registry/router alone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use openab_cp::config::CpConfig;
use openab_cp::server::{app, graceful_shutdown, sweep_leases, AppState, REASON_SHUTDOWN};

const KEY: &str = "k-primary";
const KEY_WORKER: &str = "k-worker";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn cfg(extra: &str) -> CpConfig {
    let raw = format!(
        r#"
{extra}

[[agents]]
key = "{KEY}"
namespace = "prod"
name = "koudu"
type = "primary"

[[agents]]
key = "{KEY_WORKER}"
namespace = "prod"
name = "worker-1"
type = "worker"
"#
    );
    let cfg: CpConfig = toml::from_str(&raw).expect("test config parses");
    cfg.validate().expect("test config validates");
    cfg
}

/// Start a CP on an ephemeral loopback port; returns its state and WS URL.
async fn spawn_cp(cfg: CpConfig) -> (Arc<AppState>, String) {
    let state = Arc::new(AppState::new(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (state, format!("ws://{addr}/cp"))
}

async fn connect_as(url: &str, key: &str) -> Result<Ws, WsError> {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {key}").parse().expect("header value"),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn connect(url: &str) -> Result<Ws, WsError> {
    connect_as(url, KEY).await
}

/// Connect, retrying while the identity's quota slot is still being released
/// by the server task.
async fn connect_retry(url: &str) -> Ws {
    for _ in 0..100 {
        if let Ok(ws) = connect(url).await {
            return ws;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("connection was never accepted");
}

fn register_frame(instance_id: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "cp/register",
        "params": {
            "protocol_version": 1,
            "namespace": "prod",
            "name": "koudu",
            "type": "primary",
            "instance_id": instance_id
        }
    })
    .to_string()
}

/// Send `cp/register` and return the parsed reply.
async fn register(ws: &mut Ws, instance_id: &str) -> serde_json::Value {
    ws.send(Message::Text(register_frame(instance_id).into()))
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("register must be answered")
        .expect("stream open")
        .expect("no ws error");
    serde_json::from_str(msg.to_text().unwrap()).unwrap()
}

/// How a connection ended, as observed by the client.
#[derive(Debug, PartialEq, Eq)]
enum Closed {
    /// A WS Close frame carrying a code and a reason.
    Frame { code: u16, reason: String },
    /// A Close frame with no payload — the CP never sends these on purpose.
    Bare,
    /// EOF or transport error with no Close frame at all.
    Dropped,
}

/// Wait until the peer closes the socket, and report how. `None` means the
/// socket was still open when `within` elapsed.
async fn wait_closed(ws: &mut Ws, within: Duration) -> Option<Closed> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(None) | Ok(Some(Err(_))) => return Some(Closed::Dropped),
            Ok(Some(Ok(Message::Close(Some(cf))))) => {
                return Some(Closed::Frame {
                    code: cf.code.into(),
                    reason: cf.reason.to_string(),
                })
            }
            Ok(Some(Ok(Message::Close(None)))) => return Some(Closed::Bare),
            Ok(Some(Ok(_))) => continue,
            Err(_) => continue, // read timeout: keep waiting
        }
    }
    None
}

/// The close a CP-initiated termination must carry: 1008 (policy violation)
/// plus a short reason the client can act on.
fn policy_close(reason: &str) -> Closed {
    Closed::Frame {
        code: 1008,
        reason: reason.to_string(),
    }
}

/// Body of a refused upgrade, as text.
fn http_body(resp: &tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>) -> String {
    resp.body()
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn lease_expiry_closes_the_connection_and_permits_reregistration() {
    // Deregistering on lease expiry without terminating
    // the connection left a live socket bound to a registration that no
    // longer existed — heartbeats got no reply and re-registration was
    // impossible (registration is first-frame-only). The CP must close it,
    // and the close must say why so the client can distinguish it from a
    // network drop.
    let (state, url) = spawn_cp(cfg("max_connections_per_identity = 1")).await;

    let mut ws = connect(&url).await.expect("first connection accepted");
    let ack = register(&mut ws, "i-1").await;
    assert_eq!(ack["result"]["protocol_version"], 1, "registered");
    assert_eq!(state.registry.list("prod").len(), 1);

    // Zero lease: every registration is overdue on this pass.
    sweep_leases(&state, Duration::ZERO);
    assert!(
        state.registry.list("prod").is_empty(),
        "lease expiry deregisters"
    );

    let closed = wait_closed(&mut ws, Duration::from_secs(5))
        .await
        .expect("the connection task must observe the shutdown signal and close");
    assert_eq!(
        closed,
        policy_close("lease expired"),
        "a lease-expiry close must be 1008 with a reason, not an anonymous close"
    );
    drop(ws);

    // A reconnecting client re-authenticates and registers again. This also
    // proves the connection slot was released (quota is 1 here).
    let mut ws2 = connect_retry(&url).await;
    let ack2 = register(&mut ws2, "i-2").await;
    assert_eq!(ack2["result"]["protocol_version"], 1);
    let live = state.registry.list("prod");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].instance_id, "i-2");
}

#[tokio::test]
async fn ping_only_pre_registration_socket_is_closed_at_the_deadline() {
    // Pings keep the transport alive but must not
    // extend the registration deadline — and the close must name the reason.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 1")).await;
    let mut ws = connect(&url).await.expect("connection accepted");

    let started = Instant::now();
    let mut closed = None;
    while started.elapsed() < Duration::from_secs(10) {
        let _ = ws.send(Message::Ping(vec![7].into())).await;
        if let Some(how) = wait_closed(&mut ws, Duration::from_millis(250)).await {
            closed = Some(how);
            break;
        }
    }
    let closed = closed.expect("an authenticated socket that never registers must be closed");
    assert_eq!(
        closed,
        policy_close("registration timeout"),
        "a registration-timeout close must be 1008 with a reason"
    );
    assert!(state.registry.list("prod").is_empty());
}

#[tokio::test]
async fn registration_after_the_deadline_is_not_accepted() {
    // The deadline is enforced, not merely advisory.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 1")).await;
    let mut ws = connect(&url).await.expect("connection accepted");
    tokio::time::sleep(Duration::from_millis(1_600)).await;

    let _ = ws
        .send(Message::Text(register_frame("i-late").into()))
        .await;
    let acked = match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => t.contains("effective_max_delegated_sessions"),
        _ => false,
    };
    assert!(!acked, "a late cp/register must not be acked");
    assert!(state.registry.list("prod").is_empty());
}

#[tokio::test]
async fn connection_quota_rejects_over_limit_and_recycles_on_disconnect() {
    // The quota bounds concurrent sockets per identity
    // and is released on every exit path (RAII), so connect → disconnect →
    // connect always succeeds. The refusal names the quota: 503 alone cannot
    // be told apart from an overloaded CP.
    let (state, url) = spawn_cp(cfg(
        "max_connections_per_identity = 1\nregister_timeout_secs = 30",
    ))
    .await;

    let mut ws = connect(&url).await.expect("first connection accepted");
    register(&mut ws, "i-1").await;
    assert_eq!(state.conn_count("prod/koudu"), 1);

    match connect(&url).await {
        Err(WsError::Http(resp)) => {
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "over-quota upgrade must be refused before the WS handshake"
            );
            let body = http_body(&resp);
            assert!(
                body.contains("max_connections_per_identity"),
                "the 503 body must name the quota, got {body:?}"
            );
        }
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("over-quota connection must be rejected"),
    }

    let _ = ws.close(None).await;
    drop(ws);

    let mut ws2 = connect_retry(&url).await;
    register(&mut ws2, "i-2").await;
    assert_eq!(
        state.conn_count("prod/koudu"),
        1,
        "the released slot was reused, not leaked"
    );
}

#[tokio::test]
async fn pre_registration_sockets_count_against_the_quota() {
    // The quota is taken at the upgrade, so parked
    // pre-registration sockets cannot be multiplied for free.
    let (_state, url) = spawn_cp(cfg(
        "max_connections_per_identity = 1\nregister_timeout_secs = 30",
    ))
    .await;
    let _parked = connect(&url).await.expect("connection accepted");

    match connect(&url).await {
        Err(WsError::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(http_body(&resp).contains("max_connections_per_identity"));
        }
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("an unregistered socket must still occupy its quota slot"),
    }
}

/// Register a worker that advertises `max_sessions` concurrent delegations.
async fn register_worker(ws: &mut Ws, instance_id: &str, max_sessions: u32) -> serde_json::Value {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "cp/register",
        "params": {
            "protocol_version": 1,
            "namespace": "prod",
            "name": "worker-1",
            "type": "worker",
            "instance_id": instance_id,
            "max_delegated_sessions": max_sessions
        }
    })
    .to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("register must be answered")
        .expect("stream open")
        .expect("no ws error");
    serde_json::from_str(msg.to_text().unwrap()).unwrap()
}

/// Read the next JSON frame, or `None` if none arrives within `within`.
async fn next_json(ws: &mut Ws, within: Duration) -> Option<serde_json::Value> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(None) | Ok(Some(Err(_))) => return None,
            Ok(Some(Ok(_))) => continue,
            Err(_) => continue,
        }
    }
    None
}

/// Read frames until one satisfies `want`; panics on timeout.
async fn await_frame(
    ws: &mut Ws,
    what: &str,
    want: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(v) = next_json(ws, Duration::from_millis(500)).await {
            if want(&v) {
                return v;
            }
        }
    }
    panic!("never received {what}");
}

#[tokio::test]
async fn advertised_capacity_is_clamped_by_the_global_default() {
    // The advertised budget is self-asserted and saturation is the CP's only
    // backpressure signal, so an identity with no cap of its own is clamped by
    // `default_max_delegated_sessions_cap` — there is no uncapped path.
    let (state, url) = spawn_cp(cfg(
        "register_timeout_secs = 30\ndefault_max_delegated_sessions_cap = 5",
    ))
    .await;
    let mut worker = connect_as(&url, KEY_WORKER).await.expect("worker accepted");
    let ack = register_worker(&mut worker, "i-greedy", u32::MAX).await;
    assert_eq!(
        ack["result"]["effective_max_delegated_sessions"], 5,
        "an uncapped identity advertising u32::MAX must be clamped to the global default"
    );
    assert_eq!(
        state.registry.list("prod")[0].max_delegated_sessions,
        5,
        "the clamped value is what the registry routes on"
    );
}

#[tokio::test]
async fn a_late_result_for_a_superseded_admission_is_not_delivered() {
    // Over a real pair of sockets: cancel-then-retry reuses the
    // client-supplied `delegation_id` and, with a single replica, routes to the
    // SAME worker. Before the admission token was on the wire, a late
    // `cp/delegate_result` for the cancelled admission matched the live one on
    // everything the CP checked — so its payload was delivered to the initiator
    // as the live delegation's terminal frame (masking the genuine one, per
    // "first terminal frame wins") and its commit removed the live entry.
    let (state, url) = spawn_cp(cfg(
        "register_timeout_secs = 30\nmax_connections_per_identity = 2",
    ))
    .await;

    let mut initiator = connect(&url).await.expect("initiator accepted");
    assert_eq!(
        register(&mut initiator, "i-1").await["result"]["protocol_version"],
        1
    );
    let mut worker = connect_as(&url, KEY_WORKER).await.expect("worker accepted");
    assert_eq!(
        register_worker(&mut worker, "i-w", 2).await["result"]["protocol_version"],
        1
    );

    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    let delegate = |rpc_id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": rpc_id, "method": "cp/delegate",
            "params": {
                "delegation_id": "d-1",
                "target": {"name": "worker-1"},
                "prompt": "do it",
                "deadline": deadline
            }
        })
        .to_string()
    };

    // Admission A.
    initiator
        .send(Message::Text(delegate(10).into()))
        .await
        .unwrap();
    let ack_a = await_frame(&mut initiator, "ack for A", |v| v["id"] == 10).await;
    let a = ack_a["result"]["admission"]
        .as_u64()
        .expect("the ack must carry the admission token");
    let fwd_a = await_frame(&mut worker, "forward for A", |v| {
        v["method"] == "cp/delegate"
    })
    .await;
    assert_eq!(
        fwd_a["params"]["admission"].as_u64(),
        Some(a),
        "the worker learns the token it must echo"
    );

    // The initiator cancels A, then retries the SAME id: admission B.
    // The cancel names admission A explicitly — `delegation_id` alone would
    // name whatever holds the id when the frame lands, not the work to abort.
    let cancel = serde_json::json!({
        "jsonrpc": "2.0", "id": 11, "method": "cp/cancel",
        "params": {"delegation_id": "d-1", "admission": a, "reason": "changed my mind"}
    })
    .to_string();
    initiator.send(Message::Text(cancel.into())).await.unwrap();
    assert_eq!(
        await_frame(&mut initiator, "cancel ack", |v| v["id"] == 11).await["result"]["ok"],
        true
    );

    initiator
        .send(Message::Text(delegate(12).into()))
        .await
        .unwrap();
    let ack_b = await_frame(&mut initiator, "ack for B", |v| v["id"] == 12).await;
    let b = ack_b["result"]["admission"].as_u64().expect("token on B");
    assert_ne!(a, b, "each admission of the reused id gets its own token");
    let fwd_b = await_frame(&mut worker, "forward for B", |v| {
        v["method"] == "cp/delegate" && v["params"]["admission"].as_u64() == Some(b)
    })
    .await;
    assert_eq!(fwd_b["params"]["delegation_id"], "d-1");
    assert_eq!(state.router.inflight_count(), 1, "only B is in flight");

    // The stale result for A arrives (a worker that had already computed it).
    let stale = serde_json::json!({
        "jsonrpc": "2.0", "id": 20, "method": "cp/delegate_result",
        "params": {
            "delegation_id": "d-1",
            "admission": a,
            "status": "completed",
            "result": "A's stale payload"
        }
    })
    .to_string();
    worker.send(Message::Text(stale.into())).await.unwrap();
    let ack_stale = await_frame(&mut worker, "ack for the stale result", |v| v["id"] == 20).await;
    assert_eq!(
        ack_stale["result"]["ok"], true,
        "the drop must look exactly like any other generic ack — not a distinguishable \
         reply that would reveal whether the id is currently re-admitted"
    );

    // The initiator must see nothing: B is untouched and its capacity intact.
    if let Some(v) = next_json(&mut initiator, Duration::from_millis(750)).await {
        assert_ne!(
            v["method"], "cp/delegate_result",
            "A's payload must never be delivered as B's terminal frame: {v}"
        );
    }
    assert_eq!(state.router.inflight_count(), 1, "B must remain in flight");

    // B's genuine result is then delivered, carrying B's token.
    let genuine = serde_json::json!({
        "jsonrpc": "2.0", "id": 21, "method": "cp/delegate_result",
        "params": {
            "delegation_id": "d-1",
            "admission": b,
            "status": "completed",
            "result": "B's genuine result"
        }
    })
    .to_string();
    worker.send(Message::Text(genuine.into())).await.unwrap();
    let terminal = await_frame(&mut initiator, "B's terminal frame", |v| {
        v["method"] == "cp/delegate_result"
    })
    .await;
    assert_eq!(terminal["params"]["admission"].as_u64(), Some(b));
    assert_eq!(terminal["params"]["result"], "B's genuine result");
    assert_eq!(state.router.inflight_count(), 0);
}

#[tokio::test]
async fn a_result_without_an_admission_token_is_rejected_as_malformed() {
    // The token is required, not optional: an absent token must be
    // INVALID_PARAMS rather than a wildcard matching whatever is live.
    let (state, url) = spawn_cp(cfg(
        "register_timeout_secs = 30\nmax_connections_per_identity = 2",
    ))
    .await;
    let mut initiator = connect(&url).await.expect("initiator accepted");
    register(&mut initiator, "i-1").await;
    let mut worker = connect_as(&url, KEY_WORKER).await.expect("worker accepted");
    register_worker(&mut worker, "i-w", 2).await;

    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    let del = serde_json::json!({
        "jsonrpc": "2.0", "id": 10, "method": "cp/delegate",
        "params": {
            "delegation_id": "d-1",
            "target": {"name": "worker-1"},
            "prompt": "do it",
            "deadline": deadline
        }
    })
    .to_string();
    initiator.send(Message::Text(del.into())).await.unwrap();
    await_frame(&mut initiator, "ack", |v| v["id"] == 10).await;
    await_frame(&mut worker, "forward", |v| v["method"] == "cp/delegate").await;

    let untokened = serde_json::json!({
        "jsonrpc": "2.0", "id": 30, "method": "cp/delegate_result",
        "params": {"delegation_id": "d-1", "status": "completed", "result": "no token"}
    })
    .to_string();
    worker.send(Message::Text(untokened.into())).await.unwrap();
    let reply = await_frame(&mut worker, "error for the untokened result", |v| {
        v["id"] == 30
    })
    .await;
    assert_eq!(reply["error"]["code"], -32602, "INVALID_PARAMS");
    assert_eq!(
        state.router.inflight_count(),
        1,
        "the delegation is untouched by a malformed result"
    );
    if let Some(v) = next_json(&mut initiator, Duration::from_millis(500)).await {
        assert_ne!(v["method"], "cp/delegate_result");
    }
}

#[tokio::test]
async fn a_peer_that_stops_reading_is_disconnected_and_frees_its_quota() {
    // A bounded outbound queue does not bound the WRITER. `sink.send().await`
    // inside a `select!` arm body is not cancelled by the other arms, so a peer
    // that stops reading (closed TCP receive window / half-open connection)
    // parks the connection task inside one write: it no longer observes the
    // shutdown watch, and its `ConnPermit` — released only when the task
    // returns — pins the identity's quota slots. Lease expiry cannot recover
    // that, because lease expiry works by signalling this very task.
    //
    // Every write is therefore bounded by `write_timeout_secs` and raced
    // against the shutdown signal: a timeout is a disconnect, which runs
    // teardown, drops the permit, and cancels the delegations downstream.
    const WRITE_TIMEOUT_SECS: u64 = 1;
    // Enough result payload to overrun any socket buffer on the way to a peer
    // that never reads (8 MiB inbound cap, ~4 MiB per result, 12 of them).
    const BIG: usize = 4 * 1024 * 1024;
    const RESULTS: usize = 12;

    let (state, url) = spawn_cp(cfg(&format!(
        "max_connections_per_identity = 1
register_timeout_secs = 30
write_timeout_secs = {WRITE_TIMEOUT_SECS}
max_frame_bytes = 8388608
max_result_bytes = 8388608
# Byte budget deliberately ABOVE the ~48 MiB this test enqueues: this
# regression proves the WRITE-TIMEOUT recovery path, and a budget below the
# payload would trip byte-refusal backpressure first, silently changing what
# is being tested (byte-budget refusal has its own dedicated regressions).
max_outbound_queue_bytes = 67108864"
    )))
    .await;

    // The initiator registers and then never reads again.
    let mut stalled = connect(&url).await.expect("initiator accepted");
    assert_eq!(
        register(&mut stalled, "i-stalled").await["result"]["protocol_version"],
        1
    );
    assert_eq!(state.conn_count("prod/koudu"), 1);

    // A worker that behaves normally: it serves the delegations and answers.
    let mut worker = connect_as(&url, KEY_WORKER).await.expect("worker accepted");
    assert_eq!(
        register_worker(&mut worker, "i-worker", (RESULTS + 1) as u32).await["result"]
            ["protocol_version"],
        1
    );

    // The initiator delegates RESULTS + 1 times. `d-keep` is never completed,
    // so a live delegation remains for the disconnect path to cancel.
    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    let mut ids: Vec<String> = vec!["d-keep".to_string()];
    ids.extend((0..RESULTS).map(|i| format!("d-{i}")));
    for (n, id) in ids.iter().enumerate() {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 100 + n, "method": "cp/delegate",
            "params": {
                "delegation_id": id,
                "target": {"name": "worker-1"},
                "prompt": "do it",
                "deadline": deadline
            }
        })
        .to_string();
        stalled.send(Message::Text(frame.into())).await.unwrap();
    }

    // Collect ALL forwards on the worker side first, so every delegation is
    // admitted before any large write can block the initiator's task. Each
    // forward carries the admission token the result MUST echo.
    let mut pending: Vec<(String, u64)> = Vec::new();
    while pending.len() < ids.len() {
        let msg = tokio::time::timeout(Duration::from_secs(20), worker.next())
            .await
            .expect("worker must receive its forwards")
            .expect("stream open")
            .expect("no ws error");
        let v: serde_json::Value = match msg {
            Message::Text(t) => serde_json::from_str(&t).unwrap(),
            _ => continue,
        };
        if v["method"] != "cp/delegate" {
            continue;
        }
        pending.push((
            v["params"]["delegation_id"].as_str().unwrap().to_string(),
            v["params"]["admission"]
                .as_u64()
                .expect("the forwarded frame must carry the admission token"),
        ));
    }
    assert_eq!(
        state.router.inflight_count(),
        ids.len(),
        "every delegation must be admitted before the writer is stalled"
    );

    // Answer all but `d-keep` with a large result. Each is forwarded to the
    // stalled initiator, whose socket buffers fill and whose write then blocks.
    for (n, (id, admission)) in pending.iter().enumerate() {
        if id == "d-keep" {
            continue;
        }
        let result = serde_json::json!({
            "jsonrpc": "2.0", "id": 900 + n, "method": "cp/delegate_result",
            "params": {
                "delegation_id": id,
                "admission": admission,
                "status": "completed",
                "result": "x".repeat(BIG)
            }
        })
        .to_string();
        worker.send(Message::Text(result.into())).await.unwrap();
    }

    // The stalled peer's connection task must terminate on its own — the write
    // bound is the only thing that can make it happen — releasing the quota
    // slot. Before the fix this never occurred and the assertion timed out.
    let started = Instant::now();
    let mut released = false;
    // The bound is the point of the regression: the permit must free within
    // the configured write timeout plus scheduling allowance — NOT eventually.
    // (A 30s poll here would pass even if the permit were pinned for 20s.)
    let bound = Duration::from_secs(WRITE_TIMEOUT_SECS + 4);
    while started.elapsed() < bound {
        if state.conn_count("prod/koudu") == 0 {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        released,
        "a peer that stopped reading must be disconnected within the write bound; \
         conn_count was still {} after {:?}",
        state.conn_count("prod/koudu"),
        started.elapsed()
    );

    // Downstream cancellation fires: teardown fails the initiator's remaining
    // delegation, which sends `cp/cancel` to the serving worker.
    let mut cancelled = false;
    let cancel_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < cancel_deadline {
        match tokio::time::timeout(Duration::from_millis(500), worker.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                if t.contains("cp/cancel") && t.contains("d-keep") {
                    cancelled = true;
                    break;
                }
            }
            Ok(None) | Ok(Some(Err(_))) => break,
            _ => continue,
        }
    }
    assert!(
        cancelled,
        "the stalled initiator's teardown must cancel its in-flight delegation downstream"
    );
    assert_eq!(state.router.inflight_count(), 0);

    // The quota slot is genuinely free: a fresh connection for the SAME
    // identity is accepted and can register (quota is 1 here).
    drop(stalled);
    let mut fresh = connect_retry(&url).await;
    assert_eq!(
        register(&mut fresh, "i-fresh").await["result"]["protocol_version"],
        1,
        "the released slot must be reusable by the same identity"
    );
}

// --- Graceful shutdown: SIGTERM/SIGHUP/SIGINT drain ---

/// The close every connection must see on CP shutdown: 1012 (service
/// restart — the client should reconnect) carrying the shutdown reason,
/// distinct from the 1008 policy closes the CP sends for misbehaviour.
fn shutdown_close() -> Closed {
    Closed::Frame {
        code: 1012,
        reason: REASON_SHUTDOWN.to_string(),
    }
}

#[tokio::test]
async fn shutdown_synthesizes_terminals_then_closes_with_restart() {
    // The container-stop path: every in-flight delegation gets a synthesized
    // terminal — the initiator sees `target_disconnected` and the serving
    // runtime a `cp/cancel` — riding out BEFORE the sockets close, and each
    // socket closes with a 1012 frame naming the reason instead of a bare
    // TCP reset.
    let (state, url) = spawn_cp(cfg(
        "register_timeout_secs = 30\nmax_connections_per_identity = 2\nshutdown_drain_secs = 5",
    ))
    .await;

    let mut initiator = connect(&url).await.expect("initiator accepted");
    assert_eq!(
        register(&mut initiator, "i-1").await["result"]["protocol_version"],
        1
    );
    let mut worker = connect_as(&url, KEY_WORKER).await.expect("worker accepted");
    assert_eq!(
        register_worker(&mut worker, "i-w", 2).await["result"]["protocol_version"],
        1
    );

    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    let delegate = serde_json::json!({
        "jsonrpc": "2.0", "id": 10, "method": "cp/delegate",
        "params": {
            "delegation_id": "d-1",
            "target": {"name": "worker-1"},
            "prompt": "do it",
            "deadline": deadline
        }
    })
    .to_string();
    initiator
        .send(Message::Text(delegate.into()))
        .await
        .unwrap();
    let ack = await_frame(&mut initiator, "delegate ack", |v| v["id"] == 10).await;
    let admission = ack["result"]["admission"]
        .as_u64()
        .expect("ack must carry the admission token");
    await_frame(&mut worker, "forwarded delegate", |v| {
        v["method"] == "cp/delegate"
    })
    .await;
    assert_eq!(state.router.inflight_count(), 1, "delegation in flight");

    graceful_shutdown(&state).await;

    // The synthesized terminal reaches the initiator ahead of the close.
    let terminal = await_frame(&mut initiator, "synthesized terminal", |v| {
        v["method"] == "cp/delegate_result"
    })
    .await;
    assert_eq!(terminal["params"]["delegation_id"], "d-1");
    assert_eq!(
        terminal["params"]["admission"].as_u64(),
        Some(admission),
        "the terminal must name the admission it ends"
    );
    assert_eq!(terminal["params"]["status"], "target_disconnected");
    assert_eq!(terminal["params"]["error"], REASON_SHUTDOWN);

    // The serving runtime is told to stop working on the same admission.
    let cancel = await_frame(&mut worker, "synthesized cancel", |v| {
        v["method"] == "cp/cancel"
    })
    .await;
    assert_eq!(cancel["params"]["delegation_id"], "d-1");
    assert_eq!(cancel["params"]["admission"].as_u64(), Some(admission));
    assert_eq!(cancel["params"]["reason"], REASON_SHUTDOWN);

    // Then every connection gets the restart close, not a TCP reset.
    for (ws, who) in [(&mut initiator, "initiator"), (&mut worker, "worker")] {
        let closed = wait_closed(ws, Duration::from_secs(5))
            .await
            .unwrap_or_else(|| panic!("{who} must observe a close"));
        assert_eq!(
            closed,
            shutdown_close(),
            "{who}: shutdown must close with 1012 + reason, got {closed:?}"
        );
    }

    assert_eq!(state.router.inflight_count(), 0);
    assert!(
        state.registry.list("prod").is_empty(),
        "every registration is torn down"
    );
}

#[tokio::test]
async fn shutdown_closes_sockets_that_never_registered() {
    // The per-connection close signal lives in the registry, so a socket
    // that has not completed `cp/register` is invisible to it. Shutdown must
    // still reach it: the process-wide signal is what every connection task
    // subscribes to from the upgrade on.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 30")).await;
    let mut ws = connect(&url).await.expect("connection accepted");

    graceful_shutdown(&state).await;

    let closed = wait_closed(&mut ws, Duration::from_secs(5)).await;
    assert_eq!(
        closed,
        Some(shutdown_close()),
        "a parked pre-registration socket must get the shutdown close, got {closed:?}"
    );
}

/// The temp config a spawned CP reads, removed when the guard drops — including
/// on a panicking assert, which otherwise leaves one file per failed run in
/// `/tmp`.
#[cfg(unix)]
struct CfgGuard(std::path::PathBuf);

#[cfg(unix)]
impl Drop for CfgGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Spawn a real `openab-cp` process bound to an ephemeral port; returns the
/// child, its WS URL, and a guard that cleans up its config. This is the only
/// proof the OS-level path works: `docker stop`, ECS, and k8s all deliver
/// SIGTERM before SIGKILL, and the binary used to die by default disposition
/// without ever running a shutdown path.
#[cfg(unix)]
async fn spawn_cp_process() -> (tokio::process::Child, String, CfgGuard) {
    // Grab an ephemeral port, release it, hand it to the child — the usual
    // tiny race, absorbed by `connect_retry`.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port();
    let cfg_path = std::env::temp_dir().join(format!(
        "openab-cp-shutdown-{}-{port}.toml",
        std::process::id()
    ));
    std::fs::write(
        &cfg_path,
        format!(
            "listen = \"127.0.0.1:{port}\"\n\n\
             [[agents]]\nkey = \"{KEY}\"\nnamespace = \"prod\"\nname = \"koudu\"\ntype = \"primary\"\n\n\
             [[agents]]\nkey = \"{KEY_WORKER}\"\nnamespace = \"prod\"\nname = \"worker-1\"\ntype = \"worker\"\n"
        ),
    )
    .expect("write cp config");
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_openab-cp"))
        .arg("--config")
        .arg(&cfg_path)
        // Not inherited and not piped: the child's `tracing` output would
        // otherwise interleave with the harness log, and a pipe nobody drains
        // can block the child mid-write.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn openab-cp");
    (
        child,
        format!("ws://127.0.0.1:{port}/cp"),
        CfgGuard(cfg_path),
    )
}

#[tokio::test]
async fn a_second_signal_latching_mid_close_still_sends_a_close_frame() {
    // The close a connection is promised must not be eaten by a second
    // signal latching while its frame is still on the wire: a lease sweep
    // firing during the shutdown drain (or the process signal landing
    // during a per-connection close) previously aborted the close write —
    // the peer saw a bare TCP reset instead of the Close frame. Whichever
    // shutdown path wins, some Close frame must always be observed.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 30")).await;
    let mut ws = connect(&url).await.expect("connection accepted");
    assert_eq!(
        register(&mut ws, "i-1").await["result"]["protocol_version"],
        1
    );
    let handle = state.registry.list("prod")[0].handle;

    // Latch the per-connection signal the way the sweeper does, then the
    // process-wide one immediately behind it — the second latch can land
    // while the first close frame is still being written.
    assert!(state.registry.signal_shutdown(handle, "lease expired"));
    graceful_shutdown(&state).await;

    let closed = wait_closed(&mut ws, Duration::from_secs(5)).await;
    assert_eq!(
        closed,
        Some(shutdown_close()),
        "a second signal mid-close must not reduce the peer's close frame to \
         a bare reset — nor downgrade the reason it is being told: got {closed:?}"
    );
}

#[tokio::test]
async fn a_socket_upgraded_after_the_drain_latched_is_never_registered() {
    // The drain's first act is to latch the process-wide reason; a socket that
    // completes its upgrade after that must be closed with the reason instead
    // of registering into a CP that is leaving. Deterministic here on purpose:
    // racing a real process's drain would only prove that the process was
    // usually already gone.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 30")).await;
    state.begin_shutdown(REASON_SHUTDOWN);

    let mut ws = connect(&url)
        .await
        .expect("the upgrade itself still completes");
    let closed = wait_closed(&mut ws, Duration::from_secs(5)).await;
    assert_eq!(
        closed,
        Some(shutdown_close()),
        "a socket arriving after the latch must get the shutdown close, never a \
         registration: got {closed:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_and_sighup_run_the_drain_and_exit_cleanly() {
    // SIGTERM is what every containerized deployment actually sends;
    // SIGHUP shares the path. Each must produce the graceful close on live
    // sockets and a clean exit code — never the old default-disposition kill.
    for sig in ["TERM", "HUP"] {
        let (mut child, url, _cfg) = spawn_cp_process().await;
        let mut ws = connect_retry(&url).await;
        assert_eq!(
            register(&mut ws, "i-1").await["result"]["protocol_version"],
            1
        );

        let status = std::process::Command::new("kill")
            .arg(format!("-{sig}"))
            .arg(child.id().expect("child pid").to_string())
            .status()
            .expect("kill must run");
        assert!(status.success(), "kill -{sig} failed: {status}");

        let closed = wait_closed(&mut ws, Duration::from_secs(10)).await;
        assert_eq!(
            closed,
            Some(shutdown_close()),
            "SIG{sig}: the socket must see a graceful close, got {closed:?}"
        );
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap_or_else(|_| panic!("SIG{sig}: the CP must exit"))
            .expect("wait must succeed");
        assert!(
            status.success(),
            "SIG{sig}: the CP must exit cleanly, got {status}"
        );
    }
}

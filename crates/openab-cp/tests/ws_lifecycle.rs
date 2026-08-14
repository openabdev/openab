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
use openab_cp::server::{app, sweep_leases, AppState};

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
max_result_bytes = 8388608"
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
    // admitted before any large write can block the initiator's task.
    let mut pending = Vec::new();
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
        pending.push(v["params"]["delegation_id"].as_str().unwrap().to_string());
    }
    assert_eq!(
        state.router.inflight_count(),
        ids.len(),
        "every delegation must be admitted before the writer is stalled"
    );

    // Answer all but `d-keep` with a large result. Each is forwarded to the
    // stalled initiator, whose socket buffers fill and whose write then blocks.
    for (n, id) in pending.iter().enumerate() {
        if id == "d-keep" {
            continue;
        }
        let result = serde_json::json!({
            "jsonrpc": "2.0", "id": 900 + n, "method": "cp/delegate_result",
            "params": {
                "delegation_id": id,
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

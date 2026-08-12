//! End-to-end WebSocket lifecycle tests: connection termination on lease
//! expiry (review round-3 F1) and pre-registration admission bounds
//! (review round-3 F4).
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

async fn connect(url: &str) -> Result<Ws, WsError> {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {KEY}").parse().expect("header value"),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
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

/// Wait until the peer closes the socket (Close frame, error, or EOF).
async fn wait_closed(ws: &mut Ws, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(None) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(Message::Close(_)))) => return true,
            Ok(Some(Ok(_))) => continue,
            Err(_) => continue, // read timeout: keep waiting
        }
    }
    false
}

#[tokio::test]
async fn lease_expiry_closes_the_connection_and_permits_reregistration() {
    // Review round-3 F1: deregistering on lease expiry without terminating
    // the connection left a live socket bound to a registration that no
    // longer existed — heartbeats got no reply and re-registration was
    // impossible (registration is first-frame-only). The CP must close it.
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

    assert!(
        wait_closed(&mut ws, Duration::from_secs(5)).await,
        "the connection task must observe the shutdown signal and close"
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
    // Review round-3 F4(a): pings keep the transport alive but must not
    // extend the registration deadline.
    let (state, url) = spawn_cp(cfg("register_timeout_secs = 1")).await;
    let mut ws = connect(&url).await.expect("connection accepted");

    let started = Instant::now();
    let mut closed = false;
    while started.elapsed() < Duration::from_secs(10) {
        let _ = ws.send(Message::Ping(vec![7].into())).await;
        match tokio::time::timeout(Duration::from_millis(250), ws.next()).await {
            Ok(None) | Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(Message::Close(_)))) => {
                closed = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(
        closed,
        "an authenticated socket that never registers must be closed"
    );
    assert!(state.registry.list("prod").is_empty());
}

#[tokio::test]
async fn registration_after_the_deadline_is_not_accepted() {
    // Review round-3 F4(a): the deadline is enforced, not merely advisory.
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
    // Review round-3 F4(b): the quota bounds concurrent sockets per identity
    // and is released on every exit path (RAII), so connect → disconnect →
    // connect always succeeds.
    let (state, url) = spawn_cp(cfg(
        "max_connections_per_identity = 1\nregister_timeout_secs = 30",
    ))
    .await;

    let mut ws = connect(&url).await.expect("first connection accepted");
    register(&mut ws, "i-1").await;
    assert_eq!(state.conn_count("prod/koudu"), 1);

    match connect(&url).await {
        Err(WsError::Http(resp)) => assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "over-quota upgrade must be refused before the WS handshake"
        ),
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
    // Review round-3 F4(b): the quota is taken at the upgrade, so parked
    // pre-registration sockets cannot be multiplied for free.
    let (_state, url) = spawn_cp(cfg(
        "max_connections_per_identity = 1\nregister_timeout_secs = 30",
    ))
    .await;
    let _parked = connect(&url).await.expect("connection accepted");

    match connect(&url).await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE),
        Err(e) => panic!("unexpected error: {e}"),
        Ok(_) => panic!("an unregistered socket must still occupy its quota slot"),
    }
}

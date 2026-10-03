//! Container-grade shutdown of a REAL `openab-cp` process, with work in
//! flight.
//!
//! `docker stop`, ECS and k8s all stop a task with SIGTERM and escalate to
//! SIGKILL when the grace period ends. Handling only ctrl-c/SIGINT meant every
//! deploy and scale-in killed the CP mid-flight: connected runtimes saw a TCP
//! reset instead of a Close frame, and in-flight delegations were resolved
//! only by each client's own deadline reconciliation against a vanished CP.
//!
//! Signal handling lives in `main`, so unit tests cannot reach it — but neither
//! can an in-process test of `graceful_shutdown` prove the part that actually
//! breaks in a container: that the synthesized terminals and the close frames
//! are written to the wire BEFORE the process exits. That ordering is a race
//! between the drain and process exit, and only a real process reproduces it.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const KEY: &str = "k-primary";
const KEY_WORKER: &str = "k-worker";

/// Reason the CP must put on every close frame when it shuts down, and on the
/// terminal/cancel frames it synthesizes for in-flight delegations.
///
/// Spelled out as a literal rather than imported from the crate: these are the
/// values a runtime sees on the wire, and a test that imported the constant
/// would pass even if the constant itself were wrong.
const REASON_SHUTDOWN: &str = "control plane shutting down";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Distinguishes concurrent tests' temp configs without a random dependency.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// A loopback port that was free a moment ago. The CP is started immediately
/// after, and [`Cp::connect`] absorbs the race either way.
fn free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
    let port = probe.local_addr().expect("bound address").port();
    drop(probe);
    port
}

fn config_file(port: u16, extra: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "openab-cp-sigterm-{}-{seq}.toml",
        std::process::id()
    ));
    let raw = format!(
        r#"
listen = "127.0.0.1:{port}"
register_timeout_secs = 30
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
    std::fs::write(&path, raw).expect("writing the CP config");
    path
}

/// A spawned CP process plus the temp config it reads (removed on drop).
struct Cp {
    child: Child,
    url: String,
    config: PathBuf,
}

impl Drop for Cp {
    fn drop(&mut self) {
        // `kill_on_drop` reaps the child if the test panics; the config file is
        // a temp artifact either way.
        let _ = std::fs::remove_file(&self.config);
    }
}

impl Cp {
    /// Connect as `key`, retrying while the CP is still binding its port.
    ///
    /// The retry exists because the port is grabbed and released before the
    /// child binds it. When the retries run out the child's fate is part of the
    /// panic: from the client side "the CP exited during startup" and "the CP
    /// is slow" look identical, and the exit status is the only thing that
    /// tells them apart.
    async fn connect(&mut self, key: &str) -> Ws {
        for _ in 0..100 {
            if let Ok(ws) = connect_as(&self.url, key).await {
                return ws;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let status = self.child.try_wait().expect("querying the child");
        panic!("the CP never accepted a connection (child status: {status:?})");
    }
}

/// Start the real `openab-cp` binary on a free loopback port.
async fn spawn_cp() -> Cp {
    spawn_cp_with("shutdown_drain_secs = 5").await
}

/// Start the real binary with extra config lines injected.
async fn spawn_cp_with(extra: &str) -> Cp {
    let port = free_port();
    let config = config_file(port, extra);
    let child = Command::new(env!("CARGO_BIN_EXE_openab-cp"))
        .arg("--config")
        .arg(&config)
        // Not piped: a full pipe buffer would block the child mid-write.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawning openab-cp");
    Cp {
        child,
        url: format!("ws://127.0.0.1:{port}/cp"),
        config,
    }
}

async fn connect_as(url: &str, key: &str) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = url.into_client_request().expect("client request");
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {key}").parse().expect("header value"),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

fn register_frame(name: &str, instance_id: &str) -> String {
    let ty = if name == "koudu" { "primary" } else { "worker" };
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "cp/register",
        "params": {
            "protocol_version": 1,
            "namespace": "prod",
            "name": name,
            "type": ty,
            "instance_id": instance_id
        }
    })
    .to_string()
}

/// Send `cp/register` and return the parsed ack.
async fn register(ws: &mut Ws, name: &str, instance_id: &str) -> serde_json::Value {
    ws.send(Message::Text(register_frame(name, instance_id).into()))
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("register must be answered")
        .expect("stream open")
        .expect("no ws error");
    serde_json::from_str(msg.to_text().expect("text frame")).unwrap()
}

/// Read JSON frames until one satisfies `want`; panics on timeout or a stream
/// that ends first (a reset is exactly the failure these tests exist to catch).
async fn await_frame(
    ws: &mut Ws,
    what: &str,
    want: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if want(&v) {
                    return v;
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(None) | Ok(Some(Err(_))) => panic!("stream ended while waiting for {what}"),
            Err(_) => continue,
        }
    }
    panic!("never received {what}");
}

/// The Close frame the CP must send, read as `(code, reason)`.
async fn await_close(ws: &mut Ws) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(cf))))) => {
                return (cf.code.into(), cf.reason.to_string())
            }
            Ok(Some(Ok(_))) => continue,
            Ok(None) | Ok(Some(Err(_))) => panic!("the socket dropped instead of a Close frame"),
            Err(_) => continue,
        }
    }
    panic!("never received a Close frame");
}

/// Send SIGTERM to `child`, the way every container runtime stops a task.
/// Synchronous on purpose: it must happen the instant the test asks for it,
/// with no await point in between where the CP could reach its own exit.
fn send_sigterm(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("running kill(1)");
    assert!(status.success(), "kill -TERM failed: {status}");
}

fn is_shutdown_close(code: u16, reason: &str) -> bool {
    // 1012 (service restart), not the 1008 the CP sends for misbehaviour:
    // nothing about a deploy is a policy violation, and the client should
    // reconnect rather than fix anything.
    code == 1012 && reason == REASON_SHUTDOWN
}

#[tokio::test]
async fn the_drain_budget_bounds_the_shutdown_not_the_process_lifetime() {
    // `shutdown_drain_secs` is a shutdown budget, not a lifetime: a CP with a
    // one-second drain must still be serving three seconds later, when no
    // signal has been delivered. A bound armed at startup instead of at the
    // signal would end the process on its own — turning a stop-mid-delegation
    // bug into "the CP vanishes", with no signal ever involved.
    let mut cp = spawn_cp_with("shutdown_drain_secs = 1").await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut ws = cp.connect(KEY).await;
    let ack = register(&mut ws, "koudu", "i-1").await;
    assert_eq!(
        ack["result"]["protocol_version"], 1,
        "the CP must still be serving after 3x its drain budget with no signal: {ack}"
    );

    // And the budget still applies to the shutdown itself.
    send_sigterm(cp.child.id().expect("the CP is still running"));
    let (code, reason) = await_close(&mut ws).await;
    assert!(is_shutdown_close(code, &reason), "{code} {reason:?}");
    let status = tokio::time::timeout(Duration::from_secs(20), cp.child.wait())
        .await
        .expect("the CP must exit after SIGTERM")
        .expect("reaping the CP");
    assert_eq!(status.code(), Some(0), "{status:?}");
}

/// Open a TCP connection to the CP and start an HTTP request that never
/// finishes: the headers stop one CRLF short of complete, so the server keeps
/// the connection waiting for the rest. Nothing in the CP's own shutdown can
/// complete it — it exists to prove the shutdown does not wait on it.
async fn stall_an_http_request(url: &str) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    let host = url.trim_start_matches("ws://").trim_end_matches("/cp");
    let mut stream = TcpStream::connect(host).await.expect("connecting");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("sending a partial request");
    stream
}

#[tokio::test]
async fn a_stalled_http_request_cannot_pin_the_shutdown() {
    // A CP whose drain waits for the listener's own graceful shutdown is pinned
    // by a peer that opened a socket and then said nothing: axum ends that wait
    // when the last in-flight request finishes, so the drain would never start
    // and the process would sit until the orchestrator's SIGKILL — the reset
    // this whole path exists to prevent.
    let mut cp = spawn_cp().await;
    let mut initiator = cp.connect(KEY).await;
    register(&mut initiator, "koudu", "i-1").await;
    let _stalled = stall_an_http_request(&cp.url).await;

    send_sigterm(cp.child.id().expect("the CP is still running"));

    // The drain runs regardless of the stalled request: the live socket still
    // gets its close frame.
    let (code, reason) = await_close(&mut initiator).await;
    assert!(
        is_shutdown_close(code, &reason),
        "the drain must run past a stalled request — close was {code} {reason:?}"
    );
    // And the process leaves on its own instead of waiting to be killed.
    let status = tokio::time::timeout(Duration::from_secs(30), cp.child.wait())
        .await
        .expect("a stalled request must not pin the process past the drain budget")
        .expect("reaping the CP");
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[tokio::test]
async fn a_second_signal_exits_immediately_while_the_drain_is_pending() {
    // The operator's escape hatch has to cover the whole window between the
    // first signal and the exit — including a listener that will not finish its
    // own wait. Armed only after the drain, it would never fire there.
    let mut cp = spawn_cp().await;
    let mut initiator = cp.connect(KEY).await;
    register(&mut initiator, "koudu", "i-1").await;
    let _stalled = stall_an_http_request(&cp.url).await;

    let pid = cp.child.id().expect("the CP is still running");
    send_sigterm(pid);
    // Observed close frame: the drain has started and the listener is all that
    // is left keeping the process alive.
    let (code, reason) = await_close(&mut initiator).await;
    assert!(is_shutdown_close(code, &reason), "{code} {reason:?}");

    send_sigterm(pid);

    let status = tokio::time::timeout(Duration::from_secs(20), cp.child.wait())
        .await
        .expect("a second signal must force the exit, not wait out the budget")
        .expect("reaping the CP");
    assert_eq!(
        status.code(),
        Some(143),
        "a forced exit reports 128 + SIGTERM, not a signal death: {status:?}"
    );
}

#[tokio::test]
async fn sigterm_resolves_in_flight_delegations_before_the_process_exits() {
    // The whole point of the issue, over a real socket and a real process:
    // under SIGTERM the CP must resolve the delegation it was serving, close
    // both sockets with a reason, and only then exit 0 — all inside the
    // orchestrator's grace period, before it escalates to SIGKILL.
    let mut cp = spawn_cp().await;

    let mut initiator = cp.connect(KEY).await;
    assert_eq!(
        register(&mut initiator, "koudu", "i-1").await["result"]["protocol_version"],
        1
    );
    let mut worker = cp.connect(KEY_WORKER).await;
    assert_eq!(
        register(&mut worker, "worker-1", "i-w").await["result"]["protocol_version"],
        1
    );

    // One delegation in flight and never completed.
    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    initiator
        .send(Message::Text(
            serde_json::json!({
                "jsonrpc": "2.0", "id": 10, "method": "cp/delegate",
                "params": {
                    "delegation_id": "d-1",
                    "target": {"name": "worker-1"},
                    "prompt": "do it",
                    "deadline": deadline
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let ack = await_frame(&mut initiator, "the delegation ack", |v| v["id"] == 10).await;
    assert!(ack.get("error").is_none(), "delegation accepted: {ack}");
    let admission = ack["result"]["admission"]
        .as_u64()
        .expect("the ack carries the admission token");
    await_frame(&mut worker, "the forwarded delegation", |v| {
        v["method"] == "cp/delegate"
    })
    .await;

    send_sigterm(cp.child.id().expect("the CP is still running"));

    // The initiator is told how the delegation ended, naming the admission it
    // ends — not left to reconcile its own deadline against a vanished CP.
    let terminal = await_frame(&mut initiator, "the shutdown terminal", |v| {
        v["method"] == "cp/delegate_result"
    })
    .await;
    assert_eq!(terminal["params"]["delegation_id"], "d-1");
    assert_eq!(
        terminal["params"]["status"], "target_disconnected",
        "shutdown resolves the delegation like a lost target, not a timeout: {terminal}"
    );
    assert_eq!(
        terminal["params"]["admission"].as_u64(),
        Some(admission),
        "the synthesized terminal names the admission it ends"
    );
    assert_eq!(terminal["params"]["error"], REASON_SHUTDOWN);

    // The serving runtime is told to stop working before its socket closes.
    let cancel = await_frame(&mut worker, "the shutdown cancel", |v| {
        v["method"] == "cp/cancel"
    })
    .await;
    assert_eq!(cancel["params"]["delegation_id"], "d-1");
    assert_eq!(cancel["params"]["admission"].as_u64(), Some(admission));
    assert_eq!(cancel["params"]["reason"], REASON_SHUTDOWN);

    // Both sockets get a Close frame that says why — a client can tell a
    // graceful CP shutdown from a network drop.
    let (i_code, i_reason) = await_close(&mut initiator).await;
    assert!(
        is_shutdown_close(i_code, &i_reason),
        "initiator close was {i_code} {i_reason:?}"
    );
    let (w_code, w_reason) = await_close(&mut worker).await;
    assert!(
        is_shutdown_close(w_code, &w_reason),
        "worker close was {w_code} {w_reason:?}"
    );

    // And the process leaves on its own, cleanly: an orchestrator must not
    // have to escalate to SIGKILL.
    let status = tokio::time::timeout(Duration::from_secs(30), cp.child.wait())
        .await
        .expect("the CP must exit after SIGTERM without a SIGKILL")
        .expect("reaping the CP");
    assert_eq!(
        status.code(),
        Some(0),
        "a drained shutdown exits 0, not by signal: {status:?}"
    );
}

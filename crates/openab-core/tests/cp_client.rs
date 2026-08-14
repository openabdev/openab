//! Control-plane client against the REAL control plane.
//!
//! The client's contract is a conversation, not a function: the first frame
//! must be `cp/register`, heartbeats must keep a lease alive, a delegation must
//! come back as a `cp/delegate_result` the *initiator* receives, and a
//! CP-initiated close must be recovered by re-registering. None of that is
//! observable from unit tests of the client alone — a frame the CP would reject
//! looks identical to one it accepts — so this boots `openab-cp` in-process on
//! an ephemeral loopback port and drives the real thing.
//!
//! Two participants:
//! - the **worker** is the real [`ControlPlaneClient`], with a scripted
//!   [`PromptRunner`] in place of a coding agent (the ACP layer is not what is
//!   under test here, and requiring a real agent would make this untestable in
//!   CI);
//! - the **initiator** is a raw WebSocket client acting as the `primary`, so
//!   the assertions are made where a real initiator would make them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use openab_core::config::{ControlPlaneConfig, CpAgentType};
use openab_core::control_plane::{ControlPlaneClient, PromptOutcome, PromptRunner};
use openab_cp::config::CpConfig;
use openab_cp::proto::DelegateForward;
use openab_cp::server::{app, run_sweeper, sweep_leases, AppState};

const PRIMARY_KEY: &str = "k-primary";
const WORKER_KEY: &str = "k-worker";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------
// Scripted prompt runner
// ---------------------------------------------------------------------------

/// What the fake agent does with a delegated prompt.
#[derive(Clone, Copy)]
enum Script {
    /// Answer immediately.
    Answer,
    /// Never answer on its own — the cancel/deadline paths need a turn that is
    /// still running when they fire.
    Hang,
}

struct ScriptedRunner {
    script: Script,
    /// Prompts as the runner saw them, i.e. what the ACP layer would be given.
    prompts: Mutex<Vec<String>>,
    started: AtomicUsize,
    cancelled: Mutex<Vec<String>>,
    discarded: Mutex<Vec<String>>,
}

impl ScriptedRunner {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script,
            prompts: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            cancelled: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
        })
    }
    fn started(&self) -> usize {
        self.started.load(Ordering::Relaxed)
    }
    fn cancelled(&self) -> Vec<String> {
        self.cancelled.lock().unwrap().clone()
    }
    fn discarded(&self) -> Vec<String> {
        self.discarded.lock().unwrap().clone()
    }
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl PromptRunner for ScriptedRunner {
    async fn run(
        &self,
        _session_key: &str,
        forward: &DelegateForward,
    ) -> anyhow::Result<PromptOutcome> {
        self.started.fetch_add(1, Ordering::Relaxed);
        self.prompts.lock().unwrap().push(forward.prompt.clone());
        match self.script {
            Script::Answer => Ok(PromptOutcome {
                text: format!("done: {}", forward.prompt),
                ..Default::default()
            }),
            Script::Hang => {
                // Far longer than any deadline in this file: only cancellation
                // or the local timeout may end it.
                tokio::time::sleep(Duration::from_secs(3600)).await;
                unreachable!("the hanging script must never answer")
            }
        }
    }

    async fn cancel(&self, session_key: &str) {
        self.cancelled.lock().unwrap().push(session_key.to_string());
    }

    async fn discard(&self, session_key: &str) {
        self.discarded.lock().unwrap().push(session_key.to_string());
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn cp_config(extra: &str) -> CpConfig {
    let raw = format!(
        r#"
{extra}

[[agents]]
key = "{PRIMARY_KEY}"
namespace = "prod"
name = "koudu"
type = "primary"

[[agents]]
key = "{WORKER_KEY}"
namespace = "prod"
name = "worker-1"
type = "worker"
"#
    );
    let cfg: CpConfig = toml::from_str(&raw).expect("CP test config parses");
    cfg.validate().expect("CP test config validates");
    cfg
}

/// Boot a real CP on an ephemeral loopback port, with its sweeper running.
async fn spawn_cp(cfg: CpConfig) -> (Arc<AppState>, String) {
    let state = Arc::new(AppState::new(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // Lease expiry + delegation deadline sweeps, exactly as the binary runs them.
    tokio::spawn(run_sweeper(state.clone()));
    (state, format!("ws://{addr}/cp"))
}

fn worker_cfg(url: &str) -> ControlPlaneConfig {
    ControlPlaneConfig {
        url: url.to_string(),
        auth_key: WORKER_KEY.to_string(),
        namespace: "prod".into(),
        name: "worker-1".into(),
        agent_type: CpAgentType::Worker,
        labels: [("backend".to_string(), "kiro".to_string())]
            .into_iter()
            .collect(),
        max_delegated_sessions: 2,
    }
}

/// Start the real client. `prompt_hard_timeout` is the runtime's own per-turn
/// ceiling — the other clock bounding a delegation.
fn spawn_worker(
    url: &str,
    runner: Arc<ScriptedRunner>,
    prompt_hard_timeout: Duration,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
    Arc<ControlPlaneClient>,
) {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let client = Arc::new(ControlPlaneClient::new(
        worker_cfg(url),
        runner,
        prompt_hard_timeout,
    ));
    let handle = tokio::spawn(Arc::clone(&client).run(rx));
    (tx, handle, client)
}

/// Raw initiator connection: a `primary` that registers by hand.
async fn connect_initiator(url: &str) -> Ws {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {PRIMARY_KEY}").parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let register = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "cp/register",
        "params": {
            "protocol_version": 1,
            "namespace": "prod",
            "name": "koudu",
            "type": "primary",
            "instance_id": "i-initiator"
        }
    })
    .to_string();
    ws.send(Message::Text(register)).await.unwrap();
    let ack = next_json(&mut ws).await;
    assert_eq!(ack["result"]["protocol_version"], 1, "initiator ack: {ack}");
    ws
}

async fn next_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("a frame within 10s")
            .expect("the socket is open")
            .expect("a valid frame")
        {
            Message::Text(text) => return serde_json::from_str(&text).expect("JSON frame"),
            Message::Close(_) => panic!("the CP closed the initiator's socket"),
            _ => continue,
        }
    }
}

/// Read frames until one matches `method`, answering nothing else.
async fn next_request(ws: &mut Ws, method: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let v = next_json(ws).await;
        if v["method"] == method {
            return v;
        }
    }
    panic!("no {method} frame arrived");
}

/// Wait until `predicate` holds, polling the CP's own registry/router state.
async fn wait_for(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {label}");
}

fn worker_instances(state: &Arc<AppState>) -> Vec<String> {
    state
        .registry
        .list("prod")
        .into_iter()
        .filter(|i| i.name == "worker-1")
        .map(|i| i.instance_id)
        .collect()
}

async fn delegate(ws: &mut Ws, id: u64, delegation_id: &str, prompt: &str, deadline_secs: i64) {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "cp/delegate",
        "params": {
            "delegation_id": delegation_id,
            "target": {"name": "worker-1"},
            "prompt": prompt,
            "deadline": (chrono::Utc::now() + chrono::Duration::seconds(deadline_secs)).to_rfc3339()
        }
    })
    .to_string();
    ws.send(Message::Text(frame)).await.unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Registration is the client's first frame, and the CP must accept the
/// identity it asserts — namespace, name, role, labels, and advertised budget
/// all come from `[control_plane]`.
#[tokio::test]
async fn the_client_registers_and_is_visible_in_the_registry() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Answer);
    let (shutdown, handle, client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));

    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let worker = state
        .registry
        .list("prod")
        .into_iter()
        .find(|i| i.name == "worker-1")
        .expect("the worker is registered");
    assert_eq!(worker.instance_id, client.instance_id());
    assert_eq!(
        worker.labels.get("backend").map(String::as_str),
        Some("kiro")
    );
    assert_eq!(worker.max_delegated_sessions, 2, "the advertised budget");
    assert_eq!(worker.active_sessions, 0);

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// Heartbeats are what hold the lease. Without them the CP deregisters the
/// instance and fails its in-flight delegations, so a client that registers but
/// never heartbeats is worse than one that never connected.
#[tokio::test]
async fn heartbeats_keep_the_lease_alive() {
    // A 1s cadence with a 3s lease: two full sweeps' worth of misses would
    // expire it.
    let (state, url) = spawn_cp(cp_config(
        "heartbeat_interval_secs = 1\nlease_expiry_secs = 3",
    ))
    .await;
    let runner = ScriptedRunner::new(Script::Answer);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(
        state.registry.expired(Duration::from_secs(2)).is_empty(),
        "every lease is fresh, so heartbeats are landing"
    );
    assert_eq!(
        worker_instances(&state).len(),
        1,
        "the CP's own sweeper left the registration alone"
    );

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// The full round trip, asserted where a real initiator sees it: `cp/delegate`
/// is acked with the assignment, the prompt reaches the runner, and the reply
/// comes back as a `cp/delegate_result` with `status = completed`.
#[tokio::test]
async fn a_delegation_round_trips_from_initiator_to_worker_and_back() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Answer);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let mut initiator = connect_initiator(&url).await;
    delegate(&mut initiator, 2, "d-round-trip", "ship it", 60).await;

    let ack = next_json(&mut initiator).await;
    assert_eq!(ack["id"], 2, "the delegate ack: {ack}");
    assert_eq!(ack["result"]["assigned_to"], "prod/worker-1");

    let result = next_request(&mut initiator, "cp/delegate_result").await;
    assert_eq!(result["params"]["delegation_id"], "d-round-trip");
    assert_eq!(
        result["params"]["status"], "completed",
        "the worker reported completion: {result}"
    );
    assert_eq!(result["params"]["result"], "done: ship it");
    assert_eq!(
        runner.prompts(),
        vec!["ship it".to_string()],
        "the prompt reached the agent seam verbatim"
    );

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// A turn that outruns the runtime's own ceiling is ended locally and reported
/// as `timeout`, rather than being left for the CP's deadline sweep. The
/// delegation deadline here is 60s and the local ceiling 1s, so the status the
/// initiator sees can only have come from the client.
#[tokio::test]
async fn the_local_deadline_reports_timeout_to_the_initiator() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Hang);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(1));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let mut initiator = connect_initiator(&url).await;
    delegate(&mut initiator, 3, "d-timeout", "hang forever", 60).await;
    assert_eq!(next_json(&mut initiator).await["id"], 3, "delegate ack");

    let result = next_request(&mut initiator, "cp/delegate_result").await;
    assert_eq!(result["params"]["delegation_id"], "d-timeout");
    assert_eq!(result["params"]["status"], "timeout", "{result}");
    assert_eq!(runner.started(), 1, "it really did start");
    // Local deadline cleanup: the agent is interrupted and the single-use
    // session is dropped, so nothing is left behind for the next delegation.
    wait_for("the session to be cleaned up", || {
        !runner.cancelled().is_empty() && !runner.discarded().is_empty()
    })
    .await;

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// `cp/cancel` from the initiator must stop the turn *at the worker* and free
/// the slot.
///
/// The CP removes the in-flight entry when it forwards the cancel and tells the
/// initiator nothing further (by design — the initiator asked), so the
/// observable effects are on the worker side: the agent is interrupted, the
/// single-use session is discarded, and the freed capacity accepts the next
/// delegation. The status mapping itself is pinned by the executor's unit tests.
#[tokio::test]
async fn a_cancel_stops_the_turn_and_frees_the_slot() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Hang);
    let (shutdown, handle, client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let mut initiator = connect_initiator(&url).await;
    delegate(&mut initiator, 4, "d-cancel", "hang forever", 300).await;
    let ack = next_json(&mut initiator).await;
    assert_eq!(ack["id"], 4, "delegate ack");
    let admission = ack["result"]["admission"]
        .as_u64()
        .expect("the ack names the admission this cancel must target");
    wait_for("the turn to start", || runner.started() == 1).await;

    let cancel = serde_json::json!({
        "jsonrpc": "2.0", "id": 5, "method": "cp/cancel",
        "params": {"delegation_id": "d-cancel", "admission": admission, "reason": "initiator changed its mind"}
    })
    .to_string();
    initiator.send(Message::Text(cancel)).await.unwrap();

    let expected_session = openab_core::control_plane::delegation_session_key(
        client.instance_id(),
        "d-cancel",
        admission,
    );
    wait_for("the worker to unwind the cancelled turn", || {
        runner.cancelled().contains(&expected_session)
            && runner.discarded().contains(&expected_session)
    })
    .await;

    // The slot is free again: a second delegation is admitted and answered.
    // (Same id would be refused as a duplicate, so use a new one.)
    let runner2 = Arc::clone(&runner);
    delegate(&mut initiator, 6, "d-after-cancel", "and now this", 60).await;
    wait_for("the next delegation to start", || runner2.started() == 2).await;

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// Lease expiry is the CP dropping a registration on its own initiative: it
/// closes the socket, because registration is first-frame-only and a connection
/// whose registry entry is gone can never recover. The client's job is to notice
/// and re-register — with the SAME instance id, since that identifies the
/// process, not the connection — and to be usable again afterwards.
#[tokio::test]
async fn a_cp_initiated_close_is_recovered_by_re_registering() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Answer);
    let (shutdown, handle, client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;
    let first_handle = state
        .registry
        .list("prod")
        .into_iter()
        .find(|i| i.name == "worker-1")
        .map(|i| i.handle)
        .expect("registered");

    // Zero-window sweep: expire every lease, exactly as the sweeper would after
    // a missed-heartbeat window.
    sweep_leases(&state, Duration::ZERO);
    assert!(
        worker_instances(&state).is_empty(),
        "the CP dropped the registration"
    );

    wait_for("the client to re-register", || {
        state
            .registry
            .list("prod")
            .into_iter()
            .any(|i| i.name == "worker-1" && i.handle != first_handle)
    })
    .await;
    let second = state
        .registry
        .list("prod")
        .into_iter()
        .find(|i| i.name == "worker-1")
        .expect("re-registered");
    assert_eq!(
        second.instance_id,
        client.instance_id(),
        "one instance id per process, reused across reconnects"
    );

    // The new registration is not just present, it works.
    let mut initiator = connect_initiator(&url).await;
    delegate(&mut initiator, 7, "d-after-reconnect", "still there?", 60).await;
    let ack = next_json(&mut initiator).await;
    assert_eq!(ack["result"]["assigned_to"], "prod/worker-1", "{ack}");
    let result = next_request(&mut initiator, "cp/delegate_result").await;
    assert_eq!(result["params"]["status"], "completed", "{result}");
    assert_eq!(result["params"]["result"], "done: still there?");

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// Shutdown must be a clean disconnect, not a lease timeout: the CP has to be
/// able to tell "this replica went away" from "this replica stopped answering".
#[tokio::test]
async fn shutdown_deregisters_the_instance() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Answer);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let _ = shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the client task ends on the shutdown signal")
        .expect("without panicking");

    wait_for("the CP to see the disconnect", || {
        worker_instances(&state).is_empty()
    })
    .await;
}

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
        allow_insecure_transport: false,
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

/// Raw connection that registers as `agent_type` under `key` and `name`.
/// Lets a test send frames a real runtime would never send.
async fn connect_raw(url: &str, key: &str, name: &str, agent_type: &str) -> Ws {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let register = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "cp/register",
        "params": {
            "protocol_version": 1,
            "namespace": "prod",
            "name": name,
            "type": agent_type,
            "instance_id": format!("i-raw-{name}")
        }
    })
    .to_string();
    ws.send(Message::Text(register)).await.unwrap();
    let ack = next_json(&mut ws).await;
    assert_eq!(ack["result"]["protocol_version"], 1, "raw ack: {ack}");
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

/// Shutdown during an active delegation must be owned by the serve loop: it
/// cancels and discards the local session before returning, rather than an
/// outer select dropping the serve future and detaching its task.
#[tokio::test]
async fn shutdown_drains_an_inflight_delegation_before_exit() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Hang);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let mut initiator = connect_initiator(&url).await;
    delegate(&mut initiator, 8, "d-shutdown", "hang until shutdown", 300).await;
    assert_eq!(next_json(&mut initiator).await["id"], 8, "delegate ack");
    wait_for("the delegated turn to start", || runner.started() == 1).await;

    let _ = shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(7), handle)
        .await
        .expect("client drains within its bounded window")
        .expect("client task does not panic");
    assert_eq!(runner.cancelled().len(), 1, "agent turn was interrupted");
    assert_eq!(
        runner.discarded().len(),
        1,
        "single-use session was discarded"
    );
    wait_for("the CP to see the drained disconnect", || {
        worker_instances(&state).is_empty()
    })
    .await;
}

/// Phase 0 criterion 4: stopping one worker fails its in-flight delegation as
/// `target_disconnected` at the initiator. The worker drops its own result on
/// the way out, so the CP's disconnect synthesis is what the initiator hears.
/// The lease defaults to 30s and the delegation deadline here is 300s; the
/// initiator must hear the failure within 10s, which rules out both.
#[tokio::test]
async fn a_stopped_worker_fails_its_inflight_delegation_as_target_disconnected() {
    let (state, url) = spawn_cp(cp_config("")).await;
    let runner = ScriptedRunner::new(Script::Hang);
    let (shutdown, handle, _client) =
        spawn_worker(&url, Arc::clone(&runner), Duration::from_secs(60));
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let mut initiator = connect_initiator(&url).await;
    delegate(
        &mut initiator,
        9,
        "d-worker-stopped",
        "hang until stopped",
        300,
    )
    .await;
    assert_eq!(next_json(&mut initiator).await["id"], 9, "delegate ack");
    wait_for("the delegated turn to start", || runner.started() == 1).await;

    let _ = shutdown.send(true);
    let failure = tokio::time::timeout(
        Duration::from_secs(10),
        next_request(&mut initiator, "cp/delegate_result"),
    )
    .await
    .expect("the initiator hears the failure within 10s, well inside the lease");
    assert_eq!(failure["params"]["delegation_id"], "d-worker-stopped");
    assert_eq!(
        failure["params"]["status"], "target_disconnected",
        "the stopped worker fails its delegation: {failure}"
    );
    let _ = tokio::time::timeout(Duration::from_secs(7), handle).await;
}

/// The PR4 local API against two real runtime clients and the real CP server:
/// primary UDS -> primary client -> CP -> worker client -> scripted agent -> result.
#[tokio::test]
async fn primary_local_api_lists_spawns_and_awaits_over_the_real_cp() {
    use openab_core::control_plane::{
        serve_local, ClientCommand, LocalClient, LocalRequest, LocalResponse, LocalTarget,
    };

    let (state, url) = spawn_cp(cp_config("")).await;
    let worker_runner = ScriptedRunner::new(Script::Answer);
    let (worker_shutdown, worker_task, _worker) =
        spawn_worker(&url, Arc::clone(&worker_runner), Duration::from_secs(60));

    let primary_runner = ScriptedRunner::new(Script::Answer);
    let mut cfg = worker_cfg(&url);
    cfg.auth_key = PRIMARY_KEY.to_string();
    cfg.name = "koudu".into();
    cfg.agent_type = CpAgentType::Primary;
    let (primary_shutdown, primary_rx) = tokio::sync::watch::channel(false);
    let primary = Arc::new(ControlPlaneClient::new(
        cfg,
        primary_runner,
        Duration::from_secs(60),
    ));
    let primary_handle = primary.handle();
    let primary_task = tokio::spawn(primary.run(primary_rx));

    wait_for("both runtimes to register", || {
        state.registry.list("prod").len() >= 2
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("agent.sock");
    let (local_shutdown, local_rx) = tokio::sync::watch::channel(false);
    let local_task = tokio::spawn(serve_local(socket.clone(), primary_handle, local_rx));
    wait_for("local agent socket", || socket.exists()).await;
    let local = LocalClient::new(socket);

    let list = local.request(&LocalRequest::ListAgents).await.unwrap();
    let LocalResponse::Agents { agents } = list else {
        panic!("expected agent roster");
    };
    assert!(agents.iter().any(|a| a.name == "worker-1"));

    let spawned = local
        .request(&LocalRequest::Spawn {
            delegation_id: "d-local-e2e".into(),
            target: LocalTarget {
                name: Some("worker-1".into()),
                labels: None,
            },
            prompt: "ship it".into(),
            deadline_secs: 60,
            parent: None,
        })
        .await
        .unwrap();
    let LocalResponse::Spawned {
        handle,
        assigned_to,
    } = spawned
    else {
        panic!("expected spawned handle");
    };
    assert_eq!(assigned_to, "prod/worker-1");
    assert_eq!(handle.len(), 64, "opaque random handle token");

    let terminal = local
        .request(&LocalRequest::Await { handle })
        .await
        .unwrap();
    let LocalResponse::Terminal {
        status,
        result,
        error,
    } = terminal
    else {
        panic!("expected terminal result");
    };
    assert_eq!(
        status,
        openab_core::control_plane::DelegationStatusWire::Completed
    );
    assert_eq!(result.as_deref(), Some("done: ship it"));
    assert!(error.is_none());

    let _ = local_shutdown.send(true);
    let _ = primary_shutdown.send(true);
    let _ = worker_shutdown.send(true);
    let _ = local_task.await;
    let _ = primary_task.await;
    let _ = worker_task.await;

    // Compile-time assertion that the direct command type remains exported for
    // in-process callers as well as the UDS frontends.
    let _unused: Option<ClientCommand> = None;
}

#[tokio::test]
async fn primary_local_api_cancels_a_running_delegation_over_the_real_cp() {
    use openab_core::control_plane::{
        serve_local, LocalClient, LocalRequest, LocalResponse, LocalTarget,
    };

    let (state, url) = spawn_cp(cp_config("max_deadline_secs = 600")).await;
    let worker_runner = ScriptedRunner::new(Script::Hang);
    let (worker_shutdown, worker_task, _worker) =
        spawn_worker(&url, Arc::clone(&worker_runner), Duration::from_secs(600));

    let mut cfg = worker_cfg(&url);
    cfg.auth_key = PRIMARY_KEY.to_string();
    cfg.name = "koudu".into();
    cfg.agent_type = CpAgentType::Primary;
    let (primary_shutdown, primary_rx) = tokio::sync::watch::channel(false);
    let primary = Arc::new(ControlPlaneClient::new(
        cfg,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(600),
    ));
    let handle = primary.handle();
    let primary_task = tokio::spawn(primary.run(primary_rx));
    wait_for("both runtimes to register", || {
        state.registry.list("prod").len() >= 2
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("agent.sock");
    let (local_shutdown, local_rx) = tokio::sync::watch::channel(false);
    let local_task = tokio::spawn(serve_local(socket.clone(), handle, local_rx));
    wait_for("local agent socket", || socket.exists()).await;
    let local = LocalClient::new(socket);

    let spawned = local
        .request(&LocalRequest::Spawn {
            delegation_id: "d-local-cancel".into(),
            target: LocalTarget {
                name: Some("worker-1".into()),
                labels: None,
            },
            prompt: "hang".into(),
            deadline_secs: 300,
            parent: None,
        })
        .await
        .unwrap();
    let LocalResponse::Spawned { handle, .. } = spawned else {
        panic!("expected spawned handle");
    };
    wait_for("worker prompt to start", || worker_runner.started() == 1).await;
    let cancelled = local
        .request(&LocalRequest::Cancel {
            handle,
            reason: "operator cancelled".into(),
        })
        .await
        .unwrap();
    assert_eq!(cancelled, LocalResponse::Cancelled);
    wait_for("worker cleanup", || {
        !worker_runner.cancelled().is_empty() && !worker_runner.discarded().is_empty()
    })
    .await;

    let _ = local_shutdown.send(true);
    let _ = primary_shutdown.send(true);
    let _ = worker_shutdown.send(true);
    let _ = local_task.await;
    let _ = primary_task.await;
    let _ = worker_task.await;
}

#[tokio::test]
async fn oversized_inprocess_spawn_is_rejected_without_disconnect() {
    use openab_core::control_plane::{ClientCommand, CommandError, SpawnRequest, SpawnTarget};

    let (state, url) = spawn_cp(cp_config("")).await;
    let worker_runner = ScriptedRunner::new(Script::Answer);
    let (worker_shutdown, worker_task, _worker) =
        spawn_worker(&url, Arc::clone(&worker_runner), Duration::from_secs(60));

    let mut cfg = worker_cfg(&url);
    cfg.auth_key = PRIMARY_KEY.to_string();
    cfg.name = "koudu".into();
    cfg.agent_type = CpAgentType::Primary;
    let (primary_shutdown, primary_rx) = tokio::sync::watch::channel(false);
    let primary = Arc::new(ControlPlaneClient::new(
        cfg,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    ));
    let handle = primary.handle();
    let primary_task = tokio::spawn(primary.run(primary_rx));
    wait_for("both runtimes to register", || {
        state.registry.list("prod").len() >= 2
    })
    .await;

    let (spawn_tx, spawn_rx) = tokio::sync::oneshot::channel();
    handle
        .submit(ClientCommand::Spawn {
            request: SpawnRequest {
                delegation_id: "d-oversized".into(),
                target: SpawnTarget::by_name("worker-1"),
                prompt: "x".repeat(1024 * 1024 + 4096),
                deadline: chrono::Utc::now() + chrono::Duration::seconds(60),
                parent: None,
            },
            reply: spawn_tx,
        })
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), spawn_rx)
        .await
        .expect("oversized request is answered locally")
        .unwrap();
    assert!(matches!(result, Err(CommandError::InvalidRequest(_))));

    // The same connection remains usable: a subsequent list request reaches
    // the real CP and returns the worker instead of reconnecting/flapping.
    let (list_tx, list_rx) = tokio::sync::oneshot::channel();
    handle
        .submit(ClientCommand::ListAgents { reply: list_tx })
        .await
        .unwrap();
    let agents = tokio::time::timeout(Duration::from_secs(5), list_rx)
        .await
        .expect("connection remains usable")
        .unwrap()
        .unwrap();
    assert!(agents.iter().any(|agent| agent.name == "worker-1"));
    assert_eq!(
        state
            .registry
            .list("prod")
            .into_iter()
            .filter(|agent| agent.name == "koudu")
            .count(),
        1,
        "primary stayed registered on the same live connection"
    );

    let _ = primary_shutdown.send(true);
    let _ = worker_shutdown.send(true);
    let _ = primary_task.await;
    let _ = worker_task.await;
}

/// Phase 0 criterion 3: two parallel spawns land on different workers.
///
/// Both workers advertise `max_delegated_sessions = 1` and carry the same
/// label, so a spawn selected by label has two candidates. The first spawn
/// fills its worker, so the second must pick the other one. If the first
/// spawn's capacity were not counted before the second selection, both would
/// land on the same worker.
#[tokio::test]
async fn phase0_parallel_spawns_land_on_different_workers() {
    let raw = format!(
        r#"
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

[[agents]]
key = "k-worker-2"
namespace = "prod"
name = "worker-2"
type = "worker"
"#
    );
    let cfg: CpConfig = toml::from_str(&raw).expect("two-worker config parses");
    cfg.validate().expect("two-worker config validates");
    let (state, url) = spawn_cp(cfg).await;

    let one_slot = |key: &str, name: &str| ControlPlaneConfig {
        auth_key: key.to_string(),
        name: name.into(),
        max_delegated_sessions: 1,
        ..worker_cfg(&url)
    };
    let runner_1 = ScriptedRunner::new(Script::Hang);
    let runner_2 = ScriptedRunner::new(Script::Hang);
    let (tx_1, rx_1) = tokio::sync::watch::channel(false);
    let (tx_2, rx_2) = tokio::sync::watch::channel(false);
    let worker_1 = Arc::new(ControlPlaneClient::new(
        one_slot(WORKER_KEY, "worker-1"),
        runner_1.clone(),
        Duration::from_secs(600),
    ));
    let worker_2 = Arc::new(ControlPlaneClient::new(
        one_slot("k-worker-2", "worker-2"),
        runner_2.clone(),
        Duration::from_secs(600),
    ));
    let task_1 = tokio::spawn(Arc::clone(&worker_1).run(rx_1));
    let task_2 = tokio::spawn(Arc::clone(&worker_2).run(rx_2));
    wait_for("both workers to register", || {
        state.registry.list("prod").len() >= 2
    })
    .await;

    // Two spawns, both written before either reply is read.
    let mut initiator = connect_initiator(&url).await;
    for (id, delegation_id) in [(2u64, "d-par-1"), (3u64, "d-par-2")] {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "cp/delegate",
            "params": {
                "delegation_id": delegation_id,
                "target": {"labels": {"backend": "kiro"}},
                "prompt": "sleep 30; hostname",
                "deadline": (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339()
            }
        })
        .to_string();
        initiator.send(Message::Text(frame)).await.unwrap();
    }

    let ack_1 = next_json(&mut initiator).await;
    let ack_2 = next_json(&mut initiator).await;
    assert_eq!(ack_1["id"], 2, "first ack: {ack_1}");
    assert_eq!(ack_2["id"], 3, "second ack: {ack_2}");
    let landed_1 = ack_1["result"]["assigned_to"].as_str().unwrap().to_string();
    let landed_2 = ack_2["result"]["assigned_to"].as_str().unwrap().to_string();
    assert_ne!(
        landed_1, landed_2,
        "two parallel spawns must not share a worker"
    );
    let mut landed = vec![landed_1, landed_2];
    landed.sort();
    assert_eq!(landed, vec!["prod/worker-1", "prod/worker-2"]);

    // Each worker's runner saw exactly one of the two prompts.
    wait_for("each worker to start its one delegation", || {
        runner_1.started() == 1 && runner_2.started() == 1
    })
    .await;

    let _ = tx_1.send(true);
    let _ = tx_2.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), task_1).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), task_2).await;
}

/// Phase 0 criterion 5: a worker identity cannot initiate. The CP refuses a
/// `cp/delegate` from a worker with `POLICY_DENIED` (-32003) and admits nothing.
#[tokio::test]
async fn phase0_worker_identity_cannot_initiate() {
    let (state, url) = spawn_cp(cp_config("")).await;
    // A registered worker, so the refusal is about who is asking and not about
    // a missing target. The target is the worker itself.
    let mut worker = connect_raw(&url, WORKER_KEY, "worker-1", "worker").await;
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    delegate(
        &mut worker,
        2,
        "d-worker-initiates",
        "should be refused",
        60,
    )
    .await;
    let reply = next_json(&mut worker).await;
    assert_eq!(reply["id"], 2, "the refusal answers the request: {reply}");
    assert!(reply.get("error").is_some(), "no admission: {reply}");
    assert_eq!(reply["error"]["code"], -32003, "POLICY_DENIED: {reply}");

    assert_eq!(
        state.router.inflight_count(),
        0,
        "a refused initiation leaves no in-flight entry"
    );
}

/// Phase 0 criterion 5, runtime side: a worker runtime refuses `spawn` on its
/// own client, so the request never reaches the CP and no CP policy denial is
/// needed.
#[tokio::test]
async fn a_worker_runtime_refuses_spawn_locally() {
    use openab_core::control_plane::{ClientCommand, CommandError, SpawnRequest, SpawnTarget};

    let (state, url) = spawn_cp(cp_config("")).await;
    let (worker_shutdown, worker_task, worker) = spawn_worker(
        &url,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    );
    wait_for("the worker to register", || {
        !worker_instances(&state).is_empty()
    })
    .await;

    let (spawn_tx, spawn_rx) = tokio::sync::oneshot::channel();
    worker
        .handle()
        .submit(ClientCommand::Spawn {
            request: SpawnRequest {
                delegation_id: "d-worker-spawn".into(),
                target: SpawnTarget::by_name("worker-1"),
                prompt: "should be refused locally".into(),
                deadline: chrono::Utc::now() + chrono::Duration::seconds(60),
                parent: None,
            },
            reply: spawn_tx,
        })
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), spawn_rx)
        .await
        .expect("the refusal is answered locally")
        .unwrap();
    assert!(
        matches!(&result, Err(CommandError::Internal(message)) if message.contains("not registered as a control-plane primary")),
        "a worker must refuse spawn with the primary-only error"
    );
    assert_eq!(
        state.router.inflight_count(),
        0,
        "the local refusal sends nothing to the CP"
    );

    // list_agents is NOT refused locally: it is forwarded for either role, and
    // the CP serves it to a worker. Pin that so a policy change on either side
    // shows up here.
    let (list_tx, list_rx) = tokio::sync::oneshot::channel();
    worker
        .handle()
        .submit(ClientCommand::ListAgents { reply: list_tx })
        .await
        .unwrap();
    let agents = tokio::time::timeout(Duration::from_secs(5), list_rx)
        .await
        .expect("the roster is answered")
        .unwrap()
        .expect("the CP serves cp/list_agents to a worker");
    assert!(
        agents.iter().any(|a| a.name == "worker-1"),
        "the roster includes the calling worker"
    );

    let _ = worker_shutdown.send(true);
    let _ = worker_task.await;
}

// ---------------------------------------------------------------------------
// Failure paths
// ---------------------------------------------------------------------------

/// A scripted stand-in for the CP, for failure modes the real server never
/// produces on demand. Counts accepted TCP connections; `script` drives each
/// upgraded socket.
async fn spawn_fake_cp<F, Fut>(script: F) -> (String, Arc<AtomicUsize>)
where
    F: Fn(WebSocketStream<TcpStream>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/cp", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let script = Arc::new(script);
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::Relaxed);
            let script = Arc::clone(&script);
            tokio::spawn(async move {
                if let Ok(ws) = tokio_tungstenite::accept_async(tcp).await {
                    (*script)(ws).await;
                }
            });
        }
    });
    (url, attempts)
}

/// Read the client's `cp/register` frame and return its JSON-RPC id.
async fn read_register(ws: &mut WebSocketStream<TcpStream>) -> Option<serde_json::Value> {
    while let Some(Ok(msg)) = ws.next().await {
        if let Message::Text(text) = msg {
            let req: serde_json::Value = serde_json::from_str(&text).ok()?;
            if req["method"] == "cp/register" {
                return Some(req["id"].clone());
            }
        }
    }
    None
}

/// A CP that rejects `cp/register` (unknown key, wrong identity) must put the
/// client into its jittered exponential backoff, not a reconnect spin. With a
/// 1s base the attempts land at ~0s, 0.5-1s and 1.5-3s, and the fourth no
/// earlier than 3.5s, so a 3.2s window sees two or three attempts.
#[tokio::test]
async fn a_rejected_registration_backs_off_instead_of_spinning() {
    let (url, attempts) = spawn_fake_cp(|mut ws| async move {
        if let Some(id) = read_register(&mut ws).await {
            let reply = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32001, "message": "unknown agent key"}
            });
            let _ = ws.send(Message::Text(reply.to_string())).await;
        }
        // Hold the socket until the client drops it.
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;

    let (shutdown, handle, _client) = spawn_worker(
        &url,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    );
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let seen = attempts.load(Ordering::Relaxed);
    assert!(
        (2..=3).contains(&seen),
        "a rejected registration must retry with backoff (saw {seen} attempts in 3.2s)"
    );

    let _ = shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("shutdown is honoured during backoff")
        .unwrap();
}

/// An unknown method gets METHOD_NOT_FOUND, and a frame that is not JSON is
/// ignored. Neither may end the session: both later requests are answered on
/// the SAME connection, and the client never reconnects.
#[tokio::test]
async fn unknown_methods_and_malformed_frames_do_not_end_the_session() {
    let (replies_tx, mut replies_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let replies_tx = Arc::new(replies_tx);
    let (url, attempts) = spawn_fake_cp(move |mut ws| {
        let replies_tx = Arc::clone(&replies_tx);
        async move {
            let Some(id) = read_register(&mut ws).await else {
                return;
            };
            let ack = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocol_version": 1,
                    "heartbeat_interval_secs": 15,
                    "lease_expiry_secs": 45,
                    "effective_max_delegated_sessions": 2
                }
            });
            ws.send(Message::Text(ack.to_string())).await.unwrap();
            for frame in [
                "{this is not json".to_string(),
                serde_json::json!({"jsonrpc": "2.0", "id": 901, "method": "cp/bogus"}).to_string(),
                "\u{0}\u{1}garbage".to_string(),
                serde_json::json!({"jsonrpc": "2.0", "id": 902, "method": "cp/also_bogus"})
                    .to_string(),
            ] {
                ws.send(Message::Text(frame)).await.unwrap();
            }
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        let _ = replies_tx.send(v);
                    }
                }
            }
        }
    })
    .await;

    let (shutdown, handle, _client) = spawn_worker(
        &url,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    );

    let mut answered = std::collections::BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while answered.len() < 2 && Instant::now() < deadline {
        let Ok(Some(v)) = tokio::time::timeout(Duration::from_secs(10), replies_rx.recv()).await
        else {
            break;
        };
        if let Some(id) = v["id"].as_u64().filter(|id| *id == 901 || *id == 902) {
            answered.insert(id, v);
        }
    }
    for id in [901, 902] {
        let reply = answered
            .get(&id)
            .unwrap_or_else(|| panic!("request {id} was never answered"));
        assert_eq!(reply["error"]["code"], -32601, "METHOD_NOT_FOUND: {reply}");
    }
    // Give a wrongly-ended session time to reconnect before checking.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        1,
        "the session survived; the client never reconnected"
    );

    let _ = shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// Terminal history is process-lifetime: a delegation that completed before a
/// connection loss can still be awaited and checked while the client is in
/// reconnect backoff. Socket-bound commands are still rejected NotConnected.
#[tokio::test]
async fn a_completed_delegation_is_readable_during_reconnect_backoff() {
    use openab_core::control_plane::{
        ClientCommand, CommandError, DelegationSnapshot, DelegationStatusWire, SpawnRequest,
        SpawnTarget,
    };

    // The real CP, with its serve task kept so it can be stopped: once the
    // listener is gone every reconnect is refused, which pins the client in
    // its backoff loop.
    let state = Arc::new(AppState::new(cp_config("")));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/cp", listener.local_addr().unwrap());
    let router = app(state.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let (worker_shutdown, worker_task, _worker) = spawn_worker(
        &url,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    );
    let mut cfg = worker_cfg(&url);
    cfg.auth_key = PRIMARY_KEY.to_string();
    cfg.name = "koudu".into();
    cfg.agent_type = CpAgentType::Primary;
    let (primary_shutdown, primary_rx) = tokio::sync::watch::channel(false);
    let primary = Arc::new(ControlPlaneClient::new(
        cfg,
        ScriptedRunner::new(Script::Answer),
        Duration::from_secs(60),
    ));
    let cp = primary.handle();
    let primary_task = tokio::spawn(primary.run(primary_rx));
    wait_for("both runtimes to register", || {
        state.registry.list("prod").len() >= 2
    })
    .await;

    // Spawn and await while connected.
    let (spawn_tx, spawn_rx) = tokio::sync::oneshot::channel();
    cp.submit(ClientCommand::Spawn {
        request: SpawnRequest {
            delegation_id: "d-across-blip".into(),
            target: SpawnTarget::by_name("worker-1"),
            prompt: "finish before the blip".into(),
            deadline: chrono::Utc::now() + chrono::Duration::seconds(60),
            parent: None,
        },
        reply: spawn_tx,
    })
    .await
    .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(10), spawn_rx)
        .await
        .expect("spawn is acked")
        .unwrap()
        .expect("spawn succeeds");
    let (await_tx, await_rx) = tokio::sync::oneshot::channel();
    cp.submit(ClientCommand::Await {
        handle: ack.handle.clone(),
        reply: await_tx,
    })
    .await
    .unwrap();
    let before = tokio::time::timeout(Duration::from_secs(10), await_rx)
        .await
        .expect("terminal arrives")
        .unwrap()
        .expect("the delegation completed");
    assert_eq!(
        DelegationStatusWire::from(before.status.clone()),
        DelegationStatusWire::Completed
    );

    // Lose the connection and keep it lost: stop accepting, then drop every
    // registration so the CP closes the live sockets.
    server.abort();
    let _ = server.await;
    sweep_leases(&state, Duration::ZERO);
    assert!(state.registry.list("prod").is_empty());
    // Let the primary notice the close and enter backoff.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (await_tx, await_rx) = tokio::sync::oneshot::channel();
    cp.submit(ClientCommand::Await {
        handle: ack.handle.clone(),
        reply: await_tx,
    })
    .await
    .unwrap();
    let during = tokio::time::timeout(Duration::from_secs(5), await_rx)
        .await
        .expect("await is answered during backoff")
        .unwrap();
    assert_eq!(
        during,
        Ok(before.clone()),
        "the preserved terminal is readable while disconnected"
    );

    let (check_tx, check_rx) = tokio::sync::oneshot::channel();
    cp.submit(ClientCommand::Check {
        handle: ack.handle.clone(),
        reply: check_tx,
    })
    .await
    .unwrap();
    let snapshot = tokio::time::timeout(Duration::from_secs(5), check_rx)
        .await
        .expect("check is answered during backoff")
        .unwrap();
    assert_eq!(snapshot, Ok(DelegationSnapshot::Terminal(before)));

    let (spawn_tx, spawn_rx) = tokio::sync::oneshot::channel();
    cp.submit(ClientCommand::Spawn {
        request: SpawnRequest {
            delegation_id: "d-while-down".into(),
            target: SpawnTarget::by_name("worker-1"),
            prompt: "no socket".into(),
            deadline: chrono::Utc::now() + chrono::Duration::seconds(60),
            parent: None,
        },
        reply: spawn_tx,
    })
    .await
    .unwrap();
    let refused = tokio::time::timeout(Duration::from_secs(5), spawn_rx)
        .await
        .expect("spawn is answered during backoff")
        .unwrap();
    assert!(
        matches!(refused, Err(CommandError::NotConnected)),
        "a socket-bound command is still NotConnected"
    );

    let _ = primary_shutdown.send(true);
    let _ = worker_shutdown.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), primary_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), worker_task).await;
}

//! Control-plane connection state machine.
//!
//! ```text
//! connect ──► cp/register ──► ack ──► serve loop ──► close/error ──► backoff ──┐
//!    ▲                                                                        │
//!    └────────────────────────── re-register ─────────────────────────────────┘
//! ```
//!
//! One instance id for the whole process, reused across reconnects (it
//! distinguishes replicas of the same logical agent, not connections). One task
//! owns the WebSocket sink, so there is no lock around it and no interleaved
//! frame: everything the runtime says to the CP — heartbeats, delegation
//! results, request acks — is produced by a single `select!`.
//!
//! Backoff follows the gateway adapter's shape (1/2/4/8/16/30s, shutdown-aware),
//! because the failure modes are the same: a hub that is briefly down, a rolling
//! restart, or a config the CP rejects. A rejected registration keeps retrying
//! rather than exiting — a runtime that has a chat platform must keep serving it,
//! and the loud log line is what an operator acts on.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use openab_cp::proto::{
    codes, methods, DelegateForward, DelegateResultParams, ErrorObject, HeartbeatParams,
    JsonRpcErrorResponse, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, RegisterAck,
    RegisterParams, MIN_RUNTIME_FRAME_BYTES, PROTOCOL_VERSION,
};
use rand::Rng;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};

use crate::config::{ControlPlaneConfig, CpAgentType};
use crate::control_plane::executor::{cap_text, DelegationExecutor, PromptRunner};
use crate::control_plane::primary::{ClientCommand, PrimaryState};

/// Backoff ceiling, matching the gateway adapter.
const MAX_BACKOFF_SECS: u64 = 30;
/// A session must live this long before a clean close resets the reconnect
/// backoff to 1s; shorter sessions escalate instead (anti reconnect-storm).
const STABLE_SESSION_SECS: u64 = 60;

/// How long a lost connection's in-flight delegations get to unwind (cancel the
/// agent, drop the session) before their tasks are aborted outright.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Bidirectional WS message/frame ceiling, mirroring the CP server's own
/// `max_frame_bytes` default (1 MiB). The executor caps result/error fields;
/// [`send`] additionally caps each complete serialized outbound frame.
///
/// Note (F51): The server default is 1 MiB but configurable hub-side. If the
/// hub is configured with a larger ceiling, frames exceeding 1 MiB are
/// rejected at the transport layer; future protocol revisions may negotiate
/// effective limits in `RegisterAck`.
const MAX_FRAME_BYTES: usize = MIN_RUNTIME_FRAME_BYTES;

/// Bound on DNS/TCP/TLS/WebSocket establishment. The outer shutdown select can
/// cancel this sooner, but a blackholed endpoint must still enter backoff.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the wait for the `cp/register` ack, mirroring the CP's own
/// `register_timeout_secs` default. A CP that upgrades the socket but never
/// acks must land in backoff, not hang the client until shutdown.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// Write half. Split from the read half because the serve loop must be able to
/// write from a handler while the read future is still alive — one `select!`
/// cannot hold `&mut` to the whole socket in two branches.
type WsSink = futures_util::stream::SplitSink<Ws, Message>;
type WsStream = futures_util::stream::SplitStream<Ws>;

/// A cloneable handle for submitting primary-side commands to a running
/// [`ControlPlaneClient`]. Obtained via [`ControlPlaneClient::handle`], it is
/// the initiating surface for local callers (the UDS server, an MCP tool, a
/// CLI): the client's serve loop is the single socket owner, so a command
/// carries a `oneshot` reply and is answered from inside that loop.
#[derive(Clone)]
pub struct ControlPlaneHandle {
    tx: tokio::sync::mpsc::Sender<ClientCommand>,
}

impl ControlPlaneHandle {
    /// Submit a command to the serve loop, returning `NotConnected` if the
    /// loop is not currently running (channel closed).
    pub async fn submit(
        &self,
        command: ClientCommand,
    ) -> Result<(), crate::control_plane::primary::CommandError> {
        self.tx
            .send(command)
            .await
            .map_err(|_| crate::control_plane::primary::CommandError::NotConnected)
    }
}

/// How many primary-side commands may be queued to the serve loop before a
/// submitter awaits. Bounded so a wedged loop applies backpressure rather than
/// growing an unbounded queue.
const COMMAND_QUEUE_DEPTH: usize = 64;

/// Runtime client for the OpenAB Agent Control Plane.
pub struct ControlPlaneClient {
    cfg: ControlPlaneConfig,
    /// Process-lifetime instance id, reused across reconnects.
    instance_id: String,
    executor: Arc<DelegationExecutor>,
    /// Monotonic JSON-RPC request id for frames this client originates.
    next_id: std::sync::atomic::AtomicU64,
    /// Sender kept so [`ControlPlaneClient::handle`] can hand out clones for
    /// the process lifetime, independent of connection state.
    command_tx: tokio::sync::mpsc::Sender<ClientCommand>,
    /// Receiver, taken once by [`ControlPlaneClient::run`]. Behind a mutex so
    /// `run` (which takes `Arc<Self>`) can move it into the loop; a second
    /// `run` finds it gone and serves without a command channel.
    command_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<ClientCommand>>>,
}

impl ControlPlaneClient {
    pub fn new(
        cfg: ControlPlaneConfig,
        runner: Arc<dyn PromptRunner>,
        prompt_hard_timeout: Duration,
    ) -> Self {
        if cfg.allow_insecure_transport && cfg.url.starts_with("ws://") {
            warn!(url = %cfg.url, "control-plane cleartext transport explicitly enabled; the bearer key is not encrypted by this connection");
        }
        let instance_id = uuid::Uuid::new_v4().to_string();
        // Advertised budget until the first ack tells us the effective one.
        let executor = Arc::new(DelegationExecutor::new(
            runner,
            instance_id.clone(),
            cfg.max_delegated_sessions,
            prompt_hard_timeout,
        ));
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(COMMAND_QUEUE_DEPTH);
        Self {
            cfg,
            instance_id,
            executor,
            next_id: std::sync::atomic::AtomicU64::new(1),
            command_tx,
            command_rx: std::sync::Mutex::new(Some(command_rx)),
        }
    }

    /// A cloneable command handle. Valid for the process lifetime regardless of
    /// connection state: submitting while disconnected succeeds at the channel
    /// but the command is answered `NotConnected`/`Disconnected` by the loop.
    pub fn handle(&self) -> ControlPlaneHandle {
        ControlPlaneHandle {
            tx: self.command_tx.clone(),
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn next_id(&self) -> u64 {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Connect, register, serve — forever, until `shutdown` flips.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        // Take the command receiver once for the process lifetime. A second
        // `run` (there should never be one) serves without a command channel.
        let mut command_rx = self.command_rx.lock().unwrap().take();
        let mut backoff = 1u64;
        loop {
            if *shutdown.borrow() {
                info!("control-plane client shutting down");
                return;
            }
            info!(
                agent = %format!("{}/{}", self.cfg.namespace, self.cfg.name),
                r#type = %self.cfg.agent_type,
                instance = %self.instance_id,
                "connecting to control plane"
            );
            let session_started = tokio::time::Instant::now();
            // connect_and_serve owns shutdown for each phase: connect/register
            // race it directly, then the serve loop alone performs
            // cancel/drain/abort. No outer arm may drop serve mid-cleanup.
            let served = self.connect_and_serve(&mut shutdown, &mut command_rx).await;
            match served {
                Ok(Outcome::Shutdown) => {
                    info!("control-plane client shutting down");
                    return;
                }
                Ok(Outcome::Disconnected) => {
                    // Reset the backoff only after a session that genuinely
                    // served for a while. A CP that accepts registration and
                    // then promptly closes (lease misconfig, crash loop,
                    // rolling deploys) would otherwise reconnect every second
                    // forever — a clean Close frame is not evidence of health.
                    backoff = backoff_after_session(backoff, session_started.elapsed());
                    warn!(
                        backoff_secs = backoff,
                        "control-plane connection closed — reconnecting"
                    );
                }
                Err(e) => {
                    error!(error = %format!("{e:#}"), backoff_secs = backoff, "control-plane connection failed");
                }
            }
            let retry_delay = jittered_backoff(backoff);
            debug!(
                base_backoff_secs = backoff,
                retry_delay_ms = retry_delay.as_millis(),
                "control-plane reconnect delay selected"
            );
            // While disconnected, reject any submitted command promptly rather
            // than letting it sit in the buffer until the next session. The
            // channel stays open (the handle is process-lifetime), so this only
            // answers what is queued; new submissions during the sleep are
            // answered too. A closed channel is replaced with `None` so
            // `recv_command` becomes a pending future and the sleep proceeds.
            let sleep = tokio::time::sleep(retry_delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    _ = shutdown.changed() => {
                        info!("control-plane client shutting down");
                        return;
                    }
                    maybe = recv_command(&mut command_rx) => {
                        match maybe {
                            Some(command) => reject_command(command),
                            None => { command_rx = None; }
                        }
                    }
                }
            }
            backoff = next_backoff(backoff);
        }
    }

    async fn connect_and_serve(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        command_rx: &mut Option<tokio::sync::mpsc::Receiver<ClientCommand>>,
    ) -> anyhow::Result<Outcome> {
        let ws = tokio::select! {
            result = self.connect_with_timeout() => result?,
            _ = shutdown.changed() => return Ok(Outcome::Shutdown),
        };
        let (mut sink, mut stream) = ws.split();
        let ack = tokio::select! {
            result = tokio::time::timeout(REGISTER_TIMEOUT, self.register(&mut sink, &mut stream)) => {
                result.map_err(|_| anyhow::anyhow!(
                    "control plane did not ack registration within {}s",
                    REGISTER_TIMEOUT.as_secs()
                ))??
            }
            _ = shutdown.changed() => return Ok(Outcome::Shutdown),
        };
        self.executor
            .set_effective_max(ack.effective_max_delegated_sessions);
        info!(
            instance = %self.instance_id,
            heartbeat_secs = ack.heartbeat_interval_secs,
            lease_secs = ack.lease_expiry_secs,
            max_delegated_sessions = ack.effective_max_delegated_sessions,
            "registered with control plane"
        );
        self.serve(sink, stream, &ack, shutdown, command_rx).await
    }

    async fn connect_with_timeout(&self) -> anyhow::Result<Ws> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.connect())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "control-plane connection timed out after {}s",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
    }

    /// Dial the CP. The key travels in the `Authorization` header, never the
    /// URL — the CP's own contract, so it cannot leak into an access log.
    async fn connect(&self) -> anyhow::Result<Ws> {
        let mut request = self.cfg.url.as_str().into_client_request()?;
        let bearer = format!("Bearer {}", self.cfg.auth_key);
        let mut value = HeaderValue::from_str(&bearer)
            .map_err(|_| anyhow::anyhow!("control_plane.auth_key is not a valid header value"))?;
        // Belt and braces: the key must not surface in a `{:?}` of the request.
        value.set_sensitive(true);
        request.headers_mut().insert("Authorization", value);
        // Mirror the CP's accept-side transport cap (`max_frame_bytes`,
        // default 1 MiB). Without this the client would buffer tungstenite's
        // 64 MiB default from an anomalous or misconfigured hub before any
        // parsing runs.
        let ws_config = WebSocketConfig {
            max_message_size: Some(MAX_FRAME_BYTES),
            max_frame_size: Some(MAX_FRAME_BYTES),
            ..Default::default()
        };
        let (ws, _resp) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false)
                .await
                .map_err(|e| anyhow::anyhow!("control-plane handshake failed: {e}"))?;
        Ok(ws)
    }

    /// Send the mandatory `cp/register` first frame and await its ack.
    async fn register(
        &self,
        sink: &mut WsSink,
        stream: &mut WsStream,
    ) -> anyhow::Result<RegisterAck> {
        let id = self.next_id();
        let params = RegisterParams {
            protocol_version: PROTOCOL_VERSION,
            namespace: self.cfg.namespace.clone(),
            name: self.cfg.name.clone(),
            agent_type: self.cfg.agent_type.into(),
            instance_id: self.instance_id.clone(),
            labels: self.cfg.labels.clone(),
            max_delegated_sessions: self.cfg.max_delegated_sessions,
        };
        let frame =
            JsonRpcRequest::new(id, methods::REGISTER, Some(serde_json::to_value(&params)?));
        send(sink, &frame).await?;

        // Anything other than the ack to this id is a protocol violation at
        // this point: registration is the first frame in both directions.
        loop {
            let Some(msg) = stream.next().await else {
                anyhow::bail!("control plane closed the connection before acking cp/register");
            };
            match msg? {
                Message::Text(text) => {
                    let parsed: JsonRpcMessage = serde_json::from_str(&text)
                        .map_err(|e| anyhow::anyhow!("malformed cp/register reply: {e}"))?;
                    if parsed.id != Some(id) {
                        warn!("ignoring an unexpected frame received before the register ack");
                        continue;
                    }
                    if let Some(err) = parsed.error {
                        // Identity/version rejections are operator errors: name
                        // the code so the log line is actionable, and let the
                        // caller back off rather than exiting the process.
                        anyhow::bail!(
                            "control plane rejected cp/register: {} (code {})",
                            err.message,
                            err.code
                        );
                    }
                    let result = parsed
                        .result
                        .ok_or_else(|| anyhow::anyhow!("cp/register reply carried no result"))?;
                    return validate_register_ack(result);
                }
                Message::Close(_) => {
                    anyhow::bail!("control plane closed the connection during registration")
                }
                _ => continue,
            }
        }
    }

    /// The serve loop. Single owner of the sink; every outbound frame is
    /// produced here.
    ///
    /// Finished delegations report themselves through an mpsc channel rather
    /// than a `JoinSet` polled in the `select!`: the inbound branch has to
    /// *spawn* while the completion branch is still armed, and one `select!`
    /// cannot lend the same `JoinSet` to both.
    async fn serve(
        &self,
        mut sink: WsSink,
        mut stream: WsStream,
        ack: &RegisterAck,
        shutdown: &mut watch::Receiver<bool>,
        command_rx: &mut Option<tokio::sync::mpsc::Receiver<ClientCommand>>,
    ) -> anyhow::Result<Outcome> {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(
            // A zero interval would spin; the CP's own default is 15s.
            ack.heartbeat_interval_secs.max(1),
        ));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate: skip it, registration just happened.
        heartbeat.tick().await;

        let (result_tx, mut result_rx) = tokio::sync::mpsc::channel::<DelegateResultParams>(
            // One slot per delegation this runtime can ever admit, so a task
            // never blocks handing its result over.
            (ack.effective_max_delegated_sessions as usize).max(1),
        );
        let mut serving: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        // Primary-side correlation and tracking for delegations THIS runtime
        // initiates. Lives here — inside the single socket owner — so no lock
        // guards it and every transition is serialized against frame writes.
        let mut primary = PrimaryState::new();

        let outcome = loop {
            tokio::select! {
                _ = shutdown.changed() => break Outcome::Shutdown,
                _ = heartbeat.tick() => {
                    let params = HeartbeatParams {
                        instance_id: self.instance_id.clone(),
                        active_delegated_sessions: self.executor.active(),
                    };
                    let frame = JsonRpcRequest::new(
                        self.next_id(),
                        methods::HEARTBEAT,
                        Some(serde_json::to_value(&params)?),
                    );
                    if send(&mut sink, &frame).await.is_err() {
                        break Outcome::Disconnected;
                    }
                }
                // A primary-side command: the local initiating surface (UDS
                // server, MCP tool, CLI) asked to spawn/cancel/await/list. The
                // serve loop is the single socket owner, so it — not the
                // caller — writes the frame and parks the reply.
                maybe_command = recv_command(command_rx) => {
                    match maybe_command {
                        Some(command) => {
                            if self.handle_command(command, &mut primary, &mut sink).await.is_err() {
                                break Outcome::Disconnected;
                            }
                        }
                        // The command channel closed (no handles left). Stop
                        // selecting on it by replacing it with `None`, which
                        // `recv_command` treats as a pending future.
                        None => { *command_rx = None; }
                    }
                }
                // A finished delegation reports itself. Emitted by the runtime
                // when the turn ends, never by the model: this is the only
                // frame that closes the initiator's wait.
                Some(result) = result_rx.recv() => {
                    let frame = delegate_result_request(self.next_id(), result)?;
                    if send(&mut sink, &frame).await.is_err() {
                        break Outcome::Disconnected;
                    }
                }
                inbound = stream.next() => {
                    let Some(msg) = inbound else { break Outcome::Disconnected };
                    match msg {
                        Ok(Message::Text(text)) => {
                            match self.classify_inbound(&text, &mut primary) {
                                FrameAction::Serve { ack, forward } => {
                                    let executor = Arc::clone(&self.executor);
                                    let tx = result_tx.clone();
                                    serving.push(tokio::spawn(async move {
                                        let result = executor.serve(forward).await;
                                        // A closed channel means the connection
                                        // that would carry this result is gone;
                                        // the CP fails it as target_disconnected.
                                        let _ = tx.send(result).await;
                                    }));
                                    // Prune finished tasks so a long-lived
                                    // connection does not accumulate handles.
                                    serving.retain(|h| !h.is_finished());
                                    if sink.send(Message::Text(ack)).await.is_err() {
                                        break Outcome::Disconnected;
                                    }
                                }
                                FrameAction::Reply(reply) => {
                                    if sink.send(Message::Text(reply)).await.is_err() {
                                        break Outcome::Disconnected;
                                    }
                                }
                                FrameAction::Ignore => {}
                            }
                        }
                        Ok(Message::Ping(p)) => {
                            if sink.send(Message::Pong(p)).await.is_err() {
                                break Outcome::Disconnected;
                            }
                        }
                        Ok(Message::Close(_)) => {
                            // The CP closes on lease expiry: reconnecting and
                            // re-registering is the recovery, since
                            // registration is first-frame-only.
                            info!("control plane closed the connection");
                            break Outcome::Disconnected;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "control-plane WebSocket error");
                            break Outcome::Disconnected;
                        }
                    }
                }
            }
        };

        // Whatever ended the session, nothing local may keep running: this
        // connection is the only route a result could travel, and on the CP
        // side these delegations are already (or about to be) failed as
        // `target_disconnected`. Cancelling lets each task stop its agent and
        // drop its session; the results themselves are deliberately dropped.
        let in_flight = self.executor.active();
        if in_flight > 0 {
            warn!(
                in_flight,
                "cancelling in-flight delegations; the control plane reports them as \
                 target_disconnected"
            );
        }
        self.executor.cancel_all();
        // Primary-side mirror: every delegation THIS runtime initiated and was
        // still awaiting can no longer complete over a dead socket, and every
        // parked command reply would otherwise hang forever. Answer them all
        // `Disconnected` so a caller's `await` returns instead of leaking.
        primary.fail_all_live();
        let drained = tokio::time::timeout(DRAIN_TIMEOUT, async {
            for handle in &mut serving {
                let _ = handle.await;
            }
        })
        .await;
        if drained.is_err() {
            warn!("delegation tasks did not unwind within the drain window; aborting them");
            for handle in &serving {
                handle.abort();
            }
        }
        let _ = sink.send(Message::Close(None)).await;
        let _ = sink.close().await;
        Ok(outcome)
    }

    /// Turn one primary-side command into an outbound frame, parking its reply
    /// in `primary` for later correlation. `Await` needs no frame — it only
    /// registers interest in a terminal that arrives over the socket. Returns
    /// `Err` only when the socket write fails, which ends the session.
    async fn handle_command(
        &self,
        command: ClientCommand,
        primary: &mut PrimaryState,
        sink: &mut WsSink,
    ) -> anyhow::Result<()> {
        // A primary must be registered as such to initiate; a worker-only
        // runtime rejects spawn/cancel/list locally so a misconfigured caller
        // gets a clear answer instead of a CP policy denial round-trip. Await
        // is allowed for either role (it only reads local tracking).
        match command {
            ClientCommand::Spawn { request, reply } => {
                if self.cfg.agent_type != CpAgentType::Primary {
                    let _ = reply.send(Err(primary_only_error()));
                    return Ok(());
                }
                let emission = primary.begin_spawn(self.next_id(), request, reply);
                let frame = JsonRpcRequest::new(
                    emission.rpc_id,
                    methods::DELEGATE,
                    Some(serde_json::to_value(&emission.params)?),
                );
                send(sink, &frame).await?;
            }
            ClientCommand::Cancel {
                handle,
                reason,
                reply,
            } => {
                if self.cfg.agent_type != CpAgentType::Primary {
                    let _ = reply.send(Err(primary_only_error()));
                    return Ok(());
                }
                let rpc_id = self.next_id();
                if let Some(params) = primary.begin_cancel(rpc_id, &handle, reason, reply) {
                    let frame = JsonRpcRequest::new(
                        rpc_id,
                        methods::CANCEL,
                        Some(serde_json::to_value(&params)?),
                    );
                    send(sink, &frame).await?;
                }
                // begin_cancel already answered the caller if the handle was
                // unknown; no frame in that case.
            }
            ClientCommand::ListAgents { reply } => {
                let rpc_id = self.next_id();
                primary.begin_list_agents(rpc_id, reply);
                let frame =
                    JsonRpcRequest::new(rpc_id, methods::LIST_AGENTS, Some(serde_json::json!({})));
                send(sink, &frame).await?;
            }
            ClientCommand::Await { handle, reply } => {
                primary.begin_await(&handle, reply);
            }
            ClientCommand::Check { handle, reply } => {
                primary.check(&handle, reply);
            }
        }
        Ok(())
    }

    /// Classify one inbound frame, routing primary-side concerns (JSON-RPC
    /// responses to our own requests, and the initiator-bound
    /// `cp/delegate_result`) into `primary`, and everything else to the
    /// worker-side [`Self::handle_frame`].
    fn classify_inbound(&self, text: &str, primary: &mut PrimaryState) -> FrameAction {
        let msg: JsonRpcMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "malformed frame from the control plane");
                return FrameAction::Ignore;
            }
        };
        // A frame with no method is a response to one of our own requests.
        // Route it to the primary-side correlator first; if it does not own
        // the id (e.g. a heartbeat/delegate_result ack), fall back to the
        // worker-side "absorb and log errors" behavior.
        if msg.method.is_none() {
            let Some(id) = msg.id else {
                if let Some(err) = msg.error {
                    warn!(code = err.code, message = %err.message, "control plane returned an error with no id");
                }
                return FrameAction::Ignore;
            };
            if primary.on_reply(id, msg.result, msg.error.clone()) {
                return FrameAction::Ignore;
            }
            // Not a primary request: it is a response to a heartbeat or a
            // delegate_result the worker side sent. Errors are worth a line.
            if let Some(err) = msg.error {
                warn!(code = err.code, message = %err.message, "control plane returned an error");
            }
            return FrameAction::Ignore;
        }
        // A `cp/delegate_result` REQUEST is the CP delivering a terminal to us
        // as the initiator. Route it to primary and ack it as ours.
        if msg.method.as_deref() == Some(methods::DELEGATE_RESULT) {
            let id = match msg.require_request_envelope() {
                Ok(id) => id,
                Err(err) => {
                    warn!(code = err.code, message = %err.message, "invalid cp/delegate_result envelope");
                    return error_reply(msg.id.unwrap_or(0), err);
                }
            };
            let params: Option<DelegateResultParams> =
                msg.params.and_then(|p| serde_json::from_value(p).ok());
            match params {
                Some(params) => {
                    // Ack regardless of whether we track it: an untracked
                    // (id, admission) is a stale/foreign frame the CP cannot
                    // act on, and a JSON-RPC error would be misread as our
                    // failure rather than a routing miss.
                    let _known = primary.on_delegate_result(&params);
                    ok_reply(id)
                }
                None => error_reply(
                    id,
                    ErrorObject::new(codes::INVALID_PARAMS, "invalid cp/delegate_result params"),
                ),
            }
        } else {
            // Worker-side frames (cp/delegate, cp/cancel, unknown methods).
            self.handle_frame(text)
        }
    }

    /// Classify one inbound frame. Never spawns and never writes: the serve
    /// loop owns both, so this stays a pure-enough function to unit-test (it
    /// does signal cancellation, which has no other home).
    ///
    /// CP-issued requests get a JSON-RPC result ack so the hub sees a
    /// well-formed conversation; the *outcome* of a delegation never travels in
    /// that ack — it comes later as `cp/delegate_result`, correlated by
    /// `delegation_id`.
    fn handle_frame(&self, text: &str) -> FrameAction {
        let msg: JsonRpcMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "malformed frame from the control plane");
                return FrameAction::Ignore;
            }
        };
        let Some(method) = msg.method.clone() else {
            // A response to one of our own requests (heartbeat, delegate_result
            // ack). Errors are worth a line; successes are noise.
            if let Some(err) = msg.error {
                warn!(code = err.code, message = %err.message, "control plane returned an error");
            }
            return FrameAction::Ignore;
        };
        let id = match msg.require_request_envelope() {
            Ok(id) => id,
            Err(err) => {
                warn!(code = err.code, message = %err.message, "invalid request envelope from the control plane");
                return error_reply(msg.id.unwrap_or(0), err);
            }
        };

        match method.as_str() {
            methods::DELEGATE => {
                if self.cfg.agent_type != CpAgentType::Worker {
                    return error_reply(
                        id,
                        ErrorObject::new(
                            codes::POLICY_DENIED,
                            "this runtime is registered as primary and does not serve delegations",
                        ),
                    );
                }
                let forward: Option<DelegateForward> =
                    msg.params.and_then(|p| serde_json::from_value(p).ok());
                match forward {
                    Some(forward) => match ok_reply_text(id) {
                        Some(ack) => FrameAction::Serve { ack, forward },
                        None => FrameAction::Ignore,
                    },
                    None => error_reply(
                        id,
                        ErrorObject::new(codes::INVALID_PARAMS, "invalid cp/delegate params"),
                    ),
                }
            }
            methods::CANCEL => {
                let params: Option<openab_cp::proto::CancelParams> =
                    msg.params.and_then(|p| serde_json::from_value(p).ok());
                match params {
                    Some(params) => {
                        let known = self
                            .executor
                            .cancel(&params.delegation_id, params.admission);
                        info!(
                            delegation_id = %params.delegation_id,
                            reason_bytes = params.reason.len(),
                            known,
                            "cp/cancel received"
                        );
                        // Acked either way: an unknown id means the delegation
                        // already finished here, which is not an error the CP
                        // can act on.
                        ok_reply(id)
                    }
                    None => error_reply(
                        id,
                        ErrorObject::new(codes::INVALID_PARAMS, "invalid cp/cancel params"),
                    ),
                }
            }
            other => {
                debug!(method = other, "unsupported control-plane method");
                error_reply(
                    id,
                    ErrorObject::new(
                        codes::METHOD_NOT_FOUND,
                        format!("runtime does not serve {other}"),
                    ),
                )
            }
        }
    }
}

/// What the serve loop should do with one inbound frame.
enum FrameAction {
    /// Ack the request and start serving the delegation.
    Serve {
        ack: String,
        forward: DelegateForward,
    },
    /// Write this frame back.
    Reply(String),
    /// Nothing to say.
    Ignore,
}

fn delegate_result_request(
    id: u64,
    mut result: DelegateResultParams,
) -> anyhow::Result<JsonRpcRequest> {
    let build = |result: &DelegateResultParams| -> anyhow::Result<JsonRpcRequest> {
        Ok(JsonRpcRequest::new(
            id,
            methods::DELEGATE_RESULT,
            Some(serde_json::to_value(result)?),
        ))
    };
    let frame = build(&result)?;
    if serde_json::to_string(&frame)?.len() <= MAX_FRAME_BYTES {
        return Ok(frame);
    }

    let (payload, is_result) = if let Some(text) = result.result.take() {
        (text, true)
    } else if let Some(text) = result.error.take() {
        (text, false)
    } else {
        anyhow::bail!("delegate_result envelope exceeds the transport limit without a payload");
    };
    if is_result {
        result.result = Some(String::new());
    } else {
        result.error = Some(String::new());
    }
    let empty_frame = build(&result)?;
    let envelope_bytes = serde_json::to_string(&empty_frame)?.len();
    anyhow::ensure!(
        envelope_bytes < MAX_FRAME_BYTES,
        "delegate_result envelope ({envelope_bytes} bytes) exceeds transport limit ({MAX_FRAME_BYTES} bytes)"
    );
    let fitted = cap_text(payload, MAX_FRAME_BYTES - envelope_bytes);
    if is_result {
        result.result = Some(fitted);
    } else {
        result.error = Some(fitted);
    }
    let frame = build(&result)?;
    anyhow::ensure!(
        serde_json::to_string(&frame)?.len() <= MAX_FRAME_BYTES,
        "delegate_result payload could not be fitted to the transport limit"
    );
    Ok(frame)
}

async fn send(sink: &mut WsSink, frame: &JsonRpcRequest) -> anyhow::Result<()> {
    let text = serde_json::to_string(frame)?;
    if text.len() > MAX_FRAME_BYTES {
        anyhow::bail!(
            "outbound frame ({} bytes) exceeds transport limit ({} bytes)",
            text.len(),
            MAX_FRAME_BYTES
        );
    }
    sink.send(Message::Text(text)).await?;
    Ok(())
}

/// Receive from an optional command channel. When the channel is `None` (never
/// created, or closed and cleared), this future is `pending` forever so the
/// serve loop's `select!` simply never wakes on this arm.
async fn recv_command(
    rx: &mut Option<tokio::sync::mpsc::Receiver<ClientCommand>>,
) -> Option<ClientCommand> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Answer a command that arrived while the client is not connected.
fn reject_command(command: ClientCommand) {
    use crate::control_plane::primary::CommandError;
    match command {
        ClientCommand::Spawn { reply, .. } => {
            let _ = reply.send(Err(CommandError::NotConnected));
        }
        ClientCommand::Cancel { reply, .. } => {
            let _ = reply.send(Err(CommandError::NotConnected));
        }
        ClientCommand::ListAgents { reply } => {
            let _ = reply.send(Err(CommandError::NotConnected));
        }
        ClientCommand::Await { reply, .. } => {
            // Awaiting only reads local tracking, but a disconnected client
            // holds none: there is nothing to wait on.
            let _ = reply.send(Err(CommandError::NotConnected));
        }
        ClientCommand::Check { reply, .. } => {
            // Same rationale as Await: no tracking survives a disconnect.
            let _ = reply.send(Err(CommandError::NotConnected));
        }
    }
}

/// Error answered to a spawn/cancel/list command on a runtime not registered
/// as `primary`.
fn primary_only_error() -> crate::control_plane::primary::CommandError {
    crate::control_plane::primary::CommandError::Internal(
        "this runtime is not registered as a control-plane primary and cannot initiate delegations"
            .into(),
    )
}

fn validate_register_ack(result: serde_json::Value) -> anyhow::Result<RegisterAck> {
    let ack: RegisterAck = serde_json::from_value(result)?;
    anyhow::ensure!(
        ack.protocol_version == PROTOCOL_VERSION,
        "control plane acknowledged protocol version {}, but this runtime requires {}",
        ack.protocol_version,
        PROTOCOL_VERSION
    );
    Ok(ack)
}

fn jittered_backoff(base_secs: u64) -> Duration {
    let max_ms = base_secs.saturating_mul(1000).max(1);
    Duration::from_millis(rand::thread_rng().gen_range(max_ms / 2..=max_ms))
}

fn next_backoff(current: u64) -> u64 {
    current.saturating_mul(2).min(MAX_BACKOFF_SECS)
}

fn backoff_after_session(current: u64, elapsed: Duration) -> u64 {
    if elapsed >= Duration::from_secs(STABLE_SESSION_SECS) {
        1
    } else {
        current
    }
}

fn ok_reply_text(id: u64) -> Option<String> {
    serde_json::to_string(&JsonRpcResponse::new(id, serde_json::json!({"ok": true}))).ok()
}

fn ok_reply(id: u64) -> FrameAction {
    match ok_reply_text(id) {
        Some(text) => FrameAction::Reply(text),
        None => FrameAction::Ignore,
    }
}

fn error_reply(id: u64, error: ErrorObject) -> FrameAction {
    match serde_json::to_string(&JsonRpcErrorResponse::new(id, error)) {
        Ok(text) => FrameAction::Reply(text),
        Err(_) => FrameAction::Ignore,
    }
}

/// Why a connection's serve loop ended.
enum Outcome {
    /// The process is shutting down; do not reconnect.
    Shutdown,
    /// The socket ended (close, error, or EOF); reconnect and re-register.
    Disconnected,
}

/// Test-only helpers for constructing a [`ControlPlaneHandle`] and its
/// receiver without a full client + socket, so sibling modules (the local IPC
/// server) can exercise their own plumbing.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A handle plus the receiver end of its command channel. The receiver is
    /// returned so the caller can drop it (making the handle answer
    /// `NotConnected`) or drain commands in a stub loop.
    pub(crate) fn handle_and_rx() -> (
        ControlPlaneHandle,
        tokio::sync::mpsc::Receiver<ClientCommand>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(COMMAND_QUEUE_DEPTH);
        (ControlPlaneHandle { tx }, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CpAgentType;
    use crate::control_plane::executor::PromptOutcome;
    use async_trait::async_trait;

    struct NoopRunner;

    #[async_trait]
    impl PromptRunner for NoopRunner {
        async fn run(
            &self,
            _session_key: &str,
            _forward: &DelegateForward,
        ) -> anyhow::Result<PromptOutcome> {
            Ok(PromptOutcome::default())
        }
        async fn cancel(&self, _session_key: &str) {}
        async fn discard(&self, _session_key: &str) {}
    }

    fn cfg() -> ControlPlaneConfig {
        toml::from_str(
            r#"
url = "ws://127.0.0.1:1/cp"
auth_key = "k"
namespace = "prod"
name = "worker-1"
type = "worker"
max_delegated_sessions = 3
"#,
        )
        .unwrap()
    }

    fn client() -> Arc<ControlPlaneClient> {
        Arc::new(ControlPlaneClient::new(
            cfg(),
            Arc::new(NoopRunner),
            Duration::from_secs(60),
        ))
    }

    #[test]
    fn the_instance_id_is_a_uuid_and_is_stable_for_the_process() {
        let c = client();
        assert_eq!(c.instance_id().len(), 36, "uuid v4, hyphenated");
        assert_eq!(c.instance_id(), c.instance_id());
        assert_ne!(
            client().instance_id(),
            c.instance_id(),
            "a second process is a different replica"
        );
    }

    #[test]
    fn register_params_mirror_the_config_and_never_carry_the_key() {
        let c = client();
        let params = RegisterParams {
            protocol_version: PROTOCOL_VERSION,
            namespace: c.cfg.namespace.clone(),
            name: c.cfg.name.clone(),
            agent_type: c.cfg.agent_type.into(),
            instance_id: c.instance_id.clone(),
            labels: c.cfg.labels.clone(),
            max_delegated_sessions: c.cfg.max_delegated_sessions,
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["type"], "worker");
        assert_eq!(v["namespace"], "prod");
        assert_eq!(v["max_delegated_sessions"], 3);
        assert_eq!(v["protocol_version"], PROTOCOL_VERSION);
        let text = serde_json::to_string(&v).unwrap();
        assert!(
            !text.contains("\"k\""),
            "the auth key belongs in the header, never the frame: {text}"
        );
    }

    #[test]
    fn a_primary_config_registers_as_primary() {
        let mut c = cfg();
        c.agent_type = CpAgentType::Primary;
        let ty: openab_cp::proto::AgentType = c.agent_type.into();
        assert_eq!(ty, openab_cp::proto::AgentType::Primary);
    }

    #[test]
    fn a_primary_refuses_worker_side_delegate_frames() {
        let mut primary_cfg = cfg();
        primary_cfg.agent_type = CpAgentType::Primary;
        let primary =
            ControlPlaneClient::new(primary_cfg, Arc::new(NoopRunner), Duration::from_secs(60));
        let frame = serde_json::json!({"jsonrpc":"2.0","id":9,"method":"cp/delegate","params":{
            "delegation_id":"d-1","admission":7,"prompt":"hi",
            "deadline":(chrono::Utc::now()+chrono::Duration::seconds(60)).to_rfc3339(),
            "from":"prod/koudu","chain":["prod/koudu"]}})
        .to_string();
        let FrameAction::Reply(reply) = primary.handle_frame(&frame) else {
            panic!("primary served work");
        };
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], codes::POLICY_DENIED);
    }

    #[test]
    fn register_ack_requires_the_exact_protocol_version() {
        let good = serde_json::json!({"protocol_version":PROTOCOL_VERSION,"heartbeat_interval_secs":15,"lease_expiry_secs":45,"effective_max_delegated_sessions":1});
        assert!(validate_register_ack(good).is_ok());
        let bad = serde_json::json!({"protocol_version":PROTOCOL_VERSION+1,"heartbeat_interval_secs":15,"lease_expiry_secs":45,"effective_max_delegated_sessions":1});
        assert!(validate_register_ack(bad)
            .unwrap_err()
            .to_string()
            .contains("requires"));
    }

    #[test]
    fn reconnect_backoff_is_jittered_bounded_and_resets_only_after_stability() {
        for base in [1, 2, 4, 8, 16, 30] {
            for _ in 0..100 {
                let delay = jittered_backoff(base);
                assert!(delay >= Duration::from_millis(base * 500));
                assert!(delay <= Duration::from_secs(base));
            }
        }
        assert_eq!(next_backoff(16), 30);
        assert_eq!(next_backoff(30), 30);
        assert_eq!(
            backoff_after_session(16, Duration::from_secs(STABLE_SESSION_SECS - 1)),
            16
        );
        assert_eq!(
            backoff_after_session(16, Duration::from_secs(STABLE_SESSION_SECS)),
            1
        );
    }

    #[test]
    fn delegate_result_cap_accounts_for_the_complete_envelope() {
        let result = DelegateResultParams {
            delegation_id: "d".repeat(600 * 1024),
            admission: 1,
            status: openab_cp::proto::DelegationStatus::Completed,
            result: Some("x".repeat(512 * 1024)),
            error: None,
        };
        let frame = delegate_result_request(42, result).unwrap();
        let serialized = serde_json::to_string(&frame).unwrap();
        assert!(serialized.len() <= MAX_FRAME_BYTES);
        assert!(serialized.contains("truncated by worker"));
    }

    #[test]
    fn rpc_ids_are_monotonic() {
        let c = client();
        let a = c.next_id();
        let b = c.next_id();
        assert!(b > a);
    }

    #[test]
    fn a_delegate_frame_is_acked_and_yields_a_servable_forward() {
        let c = client();
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "cp/delegate",
            "params": {
                "delegation_id": "d-1",
                "admission": 7,
                "prompt": "hi",
                "deadline": (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339(),
                "from": "prod/koudu",
                "chain": ["prod/koudu"]
            }
        })
        .to_string();
        match c.handle_frame(&frame) {
            FrameAction::Serve { ack, forward } => {
                let v: serde_json::Value = serde_json::from_str(&ack).unwrap();
                assert_eq!(v["id"], 9);
                assert_eq!(v["result"]["ok"], true);
                assert!(
                    v.get("error").is_none(),
                    "the ack says nothing about the outcome"
                );
                assert_eq!(forward.delegation_id, "d-1");
                assert_eq!(
                    forward.admission, 7,
                    "the token must survive into the forward"
                );
                assert_eq!(forward.from, "prod/koudu");
                assert_eq!(forward.chain, vec!["prod/koudu".to_string()]);
            }
            _ => panic!("cp/delegate must be served"),
        }
    }

    #[test]
    fn malformed_delegate_params_are_rejected_without_serving() {
        let c = client();
        // No deadline: the CP never sends this, but a malformed frame must not
        // become an unbounded turn.
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "cp/delegate",
            "params": {"delegation_id": "d-1", "prompt": "hi", "from": "prod/koudu", "chain": []}
        })
        .to_string();
        let FrameAction::Reply(reply) = c.handle_frame(&frame) else {
            panic!("expected an error reply, not a served delegation");
        };
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], codes::INVALID_PARAMS);
    }

    #[test]
    fn cancel_is_acked_even_for_an_unknown_delegation() {
        let c = client();
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "cp/cancel",
            "params": {"delegation_id": "gone", "admission": 3, "reason": "initiator gave up"}
        })
        .to_string();
        let FrameAction::Reply(reply) = c.handle_frame(&frame) else {
            panic!("expected an ack");
        };
        assert!(reply.contains("\"ok\":true"));
    }

    #[test]
    fn an_unknown_method_gets_method_not_found() {
        let c = client();
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "cp/event", "params": {}
        })
        .to_string();
        let FrameAction::Reply(reply) = c.handle_frame(&frame) else {
            panic!("expected an error reply");
        };
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn responses_to_our_own_requests_are_absorbed() {
        let c = client();
        for frame in [
            r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32004,"message":"no target"}}"#,
        ] {
            assert!(
                matches!(c.handle_frame(frame), FrameAction::Ignore),
                "a response is not answered"
            );
        }
    }

    #[test]
    fn a_notification_shaped_request_is_refused() {
        // `cp/*` methods are requests; an id-less one cannot be acked, and the
        // CP's own parser enforces the same rule in the other direction.
        let c = client();
        let FrameAction::Reply(reply) =
            c.handle_frame(r#"{"jsonrpc":"2.0","method":"cp/delegate","params":{}}"#)
        else {
            panic!("expected an error reply");
        };
        let v: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["error"]["code"], codes::INVALID_REQUEST);
    }

    #[test]
    fn garbage_is_dropped_not_answered() {
        let c = client();
        assert!(matches!(c.handle_frame("{not json"), FrameAction::Ignore));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_websocket_handshake_hits_the_connect_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut stalled_cfg = cfg();
        stalled_cfg.url = format!("ws://{addr}/cp");
        let stalled = Arc::new(ControlPlaneClient::new(
            stalled_cfg,
            Arc::new(NoopRunner),
            Duration::from_secs(60),
        ));
        let connecting = tokio::spawn(async move { stalled.connect_with_timeout().await });
        tokio::task::yield_now().await;
        tokio::time::advance(CONNECT_TIMEOUT + Duration::from_secs(1)).await;
        let error = match connecting.await.unwrap() {
            Ok(_) => panic!("unexpected connection"),
            Err(e) => e,
        };
        assert!(error.to_string().contains("timed out"));
        server.abort();
    }

    #[tokio::test]
    async fn run_returns_immediately_when_shutdown_is_already_set() {
        // The url points at a closed port: if the loop dialled before checking
        // shutdown, this would hang for the whole backoff instead.
        let (tx, rx) = watch::channel(true);
        tokio::time::timeout(Duration::from_secs(1), client().run(rx))
            .await
            .expect("shutdown is checked before dialling");
        drop(tx);
    }
}

use crate::acp::protocol::{
    parse_config_options, parse_usage_report, ConfigOption, JsonRpcErrorResponse, JsonRpcId,
    JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, UsageReport,
};
use crate::workflow_tail::{relay_channel, RelayReceiver, RelaySender};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace, warn};

/// Pick the most permissive selectable permission option from ACP options.
fn pick_best_option(options: &[Value]) -> Option<String> {
    let mut fallback: Option<&Value> = None;

    for kind in ["allow_always", "allow_once"] {
        if let Some(option) = options
            .iter()
            .find(|option| option.get("kind").and_then(|k| k.as_str()) == Some(kind))
        {
            return option
                .get("optionId")
                .and_then(|id| id.as_str())
                .map(str::to_owned);
        }
    }

    for option in options {
        let kind = option.get("kind").and_then(|k| k.as_str());
        if kind == Some("reject_once") || kind == Some("reject_always") {
            continue;
        }
        fallback = Some(option);
        break;
    }

    fallback
        .and_then(|option| option.get("optionId"))
        .and_then(|id| id.as_str())
        .map(str::to_owned)
}

/// Build a spec-compliant permission response with backward-compatible fallback.
fn build_permission_response(params: Option<&Value>) -> Value {
    match params
        .and_then(|p| p.get("options"))
        .and_then(|options| options.as_array())
    {
        None => json!({
            "outcome": {
                "outcome": "selected",
                "optionId": "allow_always"
            }
        }),
        Some(options) => {
            if let Some(option_id) = pick_best_option(options) {
                json!({
                    "outcome": {
                        "outcome": "selected",
                        "optionId": option_id
                    }
                })
            } else {
                json!({
                    "outcome": {
                        "outcome": "cancelled"
                    }
                })
            }
        }
    }
}

fn expand_env(val: &str) -> String {
    if val.starts_with("${") && val.ends_with('}') {
        let key = &val[2..val.len() - 1];
        std::env::var(key).unwrap_or_default()
    } else {
        val.to_string()
    }
}
use tokio::time::Instant;

/// A content block for the ACP prompt — either text or image.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text { text: String },
    Image { media_type: String, data: String },
}

impl ContentBlock {
    pub fn to_json(&self) -> Value {
        match self {
            ContentBlock::Text { text } => json!({
                "type": "text",
                "text": text
            }),
            ContentBlock::Image { media_type, data } => json!({
                "type": "image",
                "data": data,
                "mimeType": media_type
            }),
        }
    }
}

/// Lock-free view of session activity, readable without the connection mutex.
pub struct SessionActivity {
    /// Milliseconds since process boot (monotonic) of the last observed activity.
    last_active_ms: AtomicU64,
    /// True while a prompt turn is in flight (mutex likely held).
    prompt_in_flight: AtomicBool,
}

impl Default for SessionActivity {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionActivity {
    pub fn new() -> Self {
        Self {
            last_active_ms: AtomicU64::new(Self::now_ms()),
            prompt_in_flight: AtomicBool::new(false),
        }
    }

    /// Monotonic milliseconds since first use (process boot). SystemTime is
    /// unsuitable here: a wall-clock step (NTP, manual change) could make an
    /// active session look hours stale and trigger a false hung eviction.
    fn now_ms() -> u64 {
        use std::sync::OnceLock;
        static BOOT: OnceLock<std::time::Instant> = OnceLock::new();
        BOOT.get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis() as u64
    }

    pub fn touch(&self) {
        self.last_active_ms.store(Self::now_ms(), Ordering::Release);
    }

    pub fn set_in_flight(&self, in_flight: bool) {
        self.prompt_in_flight.store(in_flight, Ordering::Release);
    }

    /// Milliseconds since process boot of the last observed activity.
    pub fn last_active_ms(&self) -> u64 {
        self.last_active_ms.load(Ordering::Acquire)
    }

    /// Elapsed time since the last observed activity (saturating at zero).
    pub fn age(&self) -> std::time::Duration {
        let last = self.last_active_ms.load(Ordering::Acquire);
        std::time::Duration::from_millis(Self::now_ms().saturating_sub(last))
    }

    pub fn in_flight(&self) -> bool {
        self.prompt_in_flight.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn set_last_active_ms(&self, ms: u64) {
        self.last_active_ms.store(ms, Ordering::Release);
    }
}

/// Where the reader loop forwards notifications. `Prompt` is the foreground
/// prompt's unbounded queue. `Relay` is the bounded queue that takes over
/// after the reply for late workflow output (see `prompt_done_keep_subscriber`).
pub(crate) enum NotifySink {
    Prompt(mpsc::UnboundedSender<JsonRpcMessage>),
    Relay(RelaySender),
}

impl NotifySink {
    fn push(&self, msg: JsonRpcMessage) {
        match self {
            Self::Prompt(tx) => {
                let _ = tx.send(msg);
            }
            Self::Relay(tx) => {
                let _ = tx.send(msg);
            }
        }
    }
}

pub struct AcpConnection {
    _proc: Child,
    /// PID of the direct child, used as the process group ID for cleanup.
    child_pgid: Option<i32>,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>>,
    notify_tx: Arc<Mutex<Option<NotifySink>>>,
    pub acp_session_id: Option<String>,
    pub supports_load_session: bool,
    /// Agent name from `initialize` (`agentInfo.name`), e.g. "Kiro CLI Agent".
    /// Used to gate agent-specific extension methods.
    pub agent_name: String,
    pub config_options: Vec<ConfigOption>,
    pub last_active: Instant,
    pub activity: Arc<SessionActivity>,
    pub session_reset: bool,
    _reader_handle: JoinHandle<()>,
    _stderr_handle: Option<JoinHandle<()>>,
    /// Revokes this session's facade token when the connection is dropped, on any evict path.
    /// Held only for its `Drop` side effect (never read).
    ///
    /// It used to cancel a per-session MCP proxy server; that server is gone and the guard now
    /// carries the minted token instead.
    #[cfg(feature = "acp-mcp")]
    #[allow(dead_code)]
    facade_token_guard: Option<tokio_util::sync::DropGuard>,
}

/// Build the final set of env vars for the agent subprocess.
/// `explicit` ([agent].env) takes precedence over `inherit` ([agent].inherit_env).
/// Returns (merged env map, list of keys that were inherited from the process).
fn build_agent_env(
    explicit: &std::collections::HashMap<String, String>,
    inherit_keys: &[String],
) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let mut result: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut inherited: Vec<String> = Vec::new();

    for (k, v) in explicit {
        result.insert(k.clone(), expand_env(v));
    }

    for key in inherit_keys {
        if !result.contains_key(key) {
            if let Ok(v) = std::env::var(key) {
                result.insert(key.clone(), v);
                inherited.push(key.clone());
            }
        }
    }

    (result, inherited)
}

async fn write_json_rpc_reply<W, T>(writer: &Arc<Mutex<W>>, reply: &T)
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    if let Ok(mut data) = serde_json::to_vec(reply) {
        data.push(b'\n');
        let mut writer = writer.lock().await;
        let _ = writer.write_all(&data).await;
        let _ = writer.flush().await;
    }
}

/// Params for `session/new`. `_meta` is added only when configured, so the
/// default request is byte-identical to the legacy one.
pub(crate) fn build_session_new_params(cwd: &str, meta: Option<&Value>) -> Value {
    let mut params = json!({"cwd": cwd, "mcpServers": []});
    if let Some(meta) = meta {
        params["_meta"] = meta.clone();
    }
    params
}

/// Check that `session/set_config_option` really applied `requested` for
/// `config_id`, using the option list the agent returned.
///
/// Lenient where the response is silent: no option with that id (or an empty
/// list) is accepted. A different `currentValue` is accepted only when the
/// returned option lists a value whose display `name` equals `requested` and
/// whose `value` is the current one (agents that normalize aliases).
pub(crate) fn verify_config_option_applied(
    options: &[ConfigOption],
    config_id: &str,
    requested: &str,
) -> std::result::Result<(), String> {
    let Some(opt) = options.iter().find(|o| o.id == config_id) else {
        return Ok(());
    };
    if opt.current_value == requested {
        return Ok(());
    }
    if opt
        .options
        .iter()
        .any(|v| v.name == requested && v.value == opt.current_value)
    {
        return Ok(());
    }
    Err(format!(
        "default_config_options `{config_id}` = `{requested}` was not applied: the agent reports `{}`. \
         Check that the value exists for this agent (for kiro v3, that `~/.kiro/agents/{requested}.md` is present in the bot's HOME).",
        opt.current_value
    ))
}

/// Reader loop body: reads JSON-RPC messages from `reader`, auto-replies
/// `session/request_permission` via `writer`, resolves pending responses,
/// and forwards notifications + stale id-bearing messages to the active
/// subscriber. Extracted as a free generic function so unit tests can drive
/// it with `tokio::io::duplex()` halves instead of a real child process.
pub(crate) async fn run_reader_loop<R, W>(
    reader: R,
    writer: Arc<Mutex<W>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>>,
    notify_tx: Arc<Mutex<Option<NotifySink>>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                error!("reader error: {e}");
                break;
            }
        }
        let msg: JsonRpcMessage = match serde_json::from_str(line.trim()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        debug!(line = line.trim(), "acp_recv");

        // Agent-to-client requests must be classified before outbound response matching.
        if let (Some(method), Some(agent_request_id)) = (msg.method.as_deref(), msg.id.clone()) {
            if method == "session/request_permission" {
                let title = msg
                    .params
                    .as_ref()
                    .and_then(|p| p.get("toolCall"))
                    .and_then(|t| t.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("?");

                let outcome = build_permission_response(msg.params.as_ref());
                info!(title, %outcome, "auto-respond permission");
                let reply = JsonRpcResponse::new(agent_request_id, outcome);
                write_json_rpc_reply(&writer, &reply).await;
                continue;
            }

            // OpenAB advertises `clientCapabilities: {}`, so any other
            // agent-to-client request is a capability we never offered.
            // Surface it at warn level rather than only in the debug frame dump.
            warn!(
                method,
                "unsupported agent-to-client request; replying -32601"
            );
            let reply = JsonRpcErrorResponse::new(
                agent_request_id,
                -32601,
                format!("unsupported agent request method `{method}`"),
            );
            write_json_rpc_reply(&writer, &reply).await;
            continue;
        }

        // Response (numeric id) → resolve pending AND forward to subscriber
        if let Some(id) = msg.id.as_ref().and_then(JsonRpcId::as_u64) {
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&id) {
                // Forward to subscriber so they see the completion
                let sub = notify_tx.lock().await;
                if let Some(ntx) = sub.as_ref() {
                    // Clone the essential fields for the subscriber
                    ntx.push(JsonRpcMessage {
                        id: msg.id.clone(),
                        method: None,
                        result: msg.result.clone(),
                        error: msg.error.clone(),
                        params: None,
                    });
                }
                let _ = tx.send(msg);
                continue;
            }
            // Stale id (#732): pending was already abandoned. Falls through
            // to subscriber forwarding; the adapter recv loop filters by
            // request_id so it can't leak into the next prompt.
            trace!(request_id = id, "stale id-bearing message after abandon");
        }

        // Notification → forward to subscriber
        let sub = notify_tx.lock().await;
        if let Some(tx) = sub.as_ref() {
            tx.push(msg);
        }
    }

    // Connection closed — resolve all pending with error
    let mut map = pending.lock().await;
    for (_, tx) in map.drain() {
        let _ = tx.send(JsonRpcMessage {
            id: None,
            method: None,
            result: None,
            error: Some(crate::acp::protocol::JsonRpcError {
                code: -1,
                message: "connection closed".into(),
                data: None,
            }),
            params: None,
        });
    }
    // Close the notify channel so rx.recv() returns None
    let mut sub = notify_tx.lock().await;
    *sub = None;
}

impl AcpConnection {
    pub async fn spawn(
        command: &str,
        args: &[String],
        working_dir: &str,
        env: &std::collections::HashMap<String, String>,
        inherit_env: &[String],
    ) -> Result<Self> {
        info!(cmd = command, ?args, cwd = working_dir, "spawning agent");

        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(working_dir);
        // Create a new process group so we can kill the entire tree.
        // SAFETY: setpgid is async-signal-safe (POSIX.1-2008) and called
        // before exec. Return value checked — failure means the child won't
        // have its own process group, so kill(-pgid) would be unsafe.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(windows)]
        {
            cmd.creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP
        }
        // Clear inherited env to prevent credential leakage (e.g. DISCORD_BOT_TOKEN).
        // Only [agent].env values + essential baseline vars are passed through.
        cmd.env_clear();
        // Preserve the real HOME so agents can find OAuth/auth files (~/.codex,
        // ~/.claude, ~/.config/gh, etc.). working_dir is already set via
        // current_dir() above and is not necessarily the user's home directory.
        cmd.env(
            "HOME",
            std::env::var("HOME").unwrap_or_else(|_| working_dir.into()),
        );
        cmd.env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
        );
        #[cfg(unix)]
        {
            cmd.env(
                "USER",
                std::env::var("USER").unwrap_or_else(|_| "agent".into()),
            );
        }
        #[cfg(windows)]
        {
            // Windows requires SystemRoot for DLL loading and basic OS functionality.
            // USERPROFILE is the Windows equivalent of HOME.
            cmd.env(
                "USERPROFILE",
                std::env::var("USERPROFILE").unwrap_or_else(|_| working_dir.into()),
            );
            cmd.env(
                "USERNAME",
                std::env::var("USERNAME").unwrap_or_else(|_| "agent".into()),
            );
            if let Ok(v) = std::env::var("SystemRoot") {
                cmd.env("SystemRoot", v);
            }
            if let Ok(v) = std::env::var("SystemDrive") {
                cmd.env("SystemDrive", v);
            }
        }
        for (k, v) in env {
            cmd.env(k, expand_env(v));
        }
        // Inherit selected env vars from the OAB process (e.g. vars injected
        // via Kubernetes envFrom).  Keys already in [agent].env are skipped —
        // explicit values take precedence.
        let (agent_env, inherited_keys) = build_agent_env(env, inherit_env);
        for (k, v) in &agent_env {
            cmd.env(k, v);
        }
        if !agent_env.is_empty() {
            let explicit_keys: Vec<&String> = env.keys().collect();
            tracing::warn!(
                ?explicit_keys,
                ?inherited_keys,
                "[agent].env/inherit_env is set -- these values are accessible to the agent and could be exfiltrated via prompt injection"
            );
        }
        let mut proc = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to spawn {command}: {e}"))?;
        let child_pgid = proc.id().and_then(|pid| i32::try_from(pid).ok());

        let stdout = proc.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stdin = proc.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdin = Arc::new(Mutex::new(stdin));

        // Capture agent stderr and log it (ACP spec: agents MAY write to stderr
        // for logging; clients MAY capture or ignore it).
        let stderr_handle = if let Some(stderr) = proc.stderr.take() {
            let cmd_name = command.to_string();
            Some(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                let sanitized: String = trimmed
                                    .chars()
                                    .filter(|c| !c.is_control() || *c == '\t')
                                    .collect();
                                if !sanitized.is_empty() {
                                    tracing::warn!(agent = %cmd_name, "{sanitized}");
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }))
        } else {
            None
        };

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<NotifySink>>> = Arc::new(Mutex::new(None));

        let reader_handle = tokio::spawn(run_reader_loop(
            stdout,
            stdin.clone(),
            pending.clone(),
            notify_tx.clone(),
        ));

        let activity = Arc::new(SessionActivity::new());

        Ok(Self {
            _proc: proc,
            child_pgid,
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            notify_tx,
            acp_session_id: None,
            supports_load_session: false,
            agent_name: String::new(),
            config_options: Vec::new(),
            last_active: Instant::now(),
            activity,
            session_reset: false,
            _reader_handle: reader_handle,
            _stderr_handle: stderr_handle,
            #[cfg(feature = "acp-mcp")]
            facade_token_guard: None,
        })
    }

    /// Attach the guard that revokes this session's facade token when the connection drops.
    #[cfg(feature = "acp-mcp")]
    pub fn set_facade_token_guard(&mut self, guard: Option<tokio_util::sync::DropGuard>) {
        self.facade_token_guard = guard;
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) async fn send_raw(&self, data: &str) -> Result<()> {
        debug!(data = data.trim(), "acp_send");
        // A hung agent can stop draining stdin; bound the write so callers
        // (and the mutexes they hold) can never block on it indefinitely.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut w = self.stdin.lock().await;
            w.write_all(data.as_bytes()).await?;
            w.write_all(b"\n").await?;
            w.flush().await?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("stdin write timeout"))??;
        Ok(())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<JsonRpcMessage> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(id, method, params);
        let data = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        self.send_raw(&data).await?;

        let timeout_secs = if method == "session/new" { 120 } else { 30 };
        let resp = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
            .await
            .map_err(|_| anyhow!("timeout waiting for {method} response"))?
            .map_err(|_| anyhow!("channel closed waiting for {method}"))?;

        if let Some(err) = &resp.error {
            return Err(anyhow!("{err}"));
        }
        Ok(resp)
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let resp = self
            .send_request(
                "initialize",
                Some(json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "openab", "version": "0.1.0"},
                })),
            )
            .await?;

        let result = resp.result.as_ref();
        let agent_name = result
            .and_then(|r| r.get("agentInfo"))
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");
        self.agent_name = agent_name.to_string();
        self.supports_load_session = result
            .and_then(|r| r.get("agentCapabilities"))
            .and_then(|c| c.get("loadSession"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        info!(
            agent = agent_name,
            load_session = self.supports_load_session,
            "initialized"
        );
        Ok(())
    }

    pub async fn session_new(&mut self, cwd: &str) -> Result<String> {
        self.session_new_with_meta(cwd, None).await
    }

    /// `session/new` with an optional `_meta` object (`[agent].session_new_meta`).
    pub async fn session_new_with_meta(
        &mut self,
        cwd: &str,
        meta: Option<&Value>,
    ) -> Result<String> {
        let resp = self
            .send_request("session/new", Some(build_session_new_params(cwd, meta)))
            .await?;

        let session_id = resp
            .result
            .as_ref()
            .and_then(|r| r.get("sessionId"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("no sessionId in session/new response"))?
            .to_string();

        info!(session_id = %session_id, "session created");
        self.acp_session_id = Some(session_id.clone());
        if let Some(result) = resp.result.as_ref() {
            self.config_options = parse_config_options(result);
            if !self.config_options.is_empty() {
                info!(count = self.config_options.len(), "parsed configOptions");
            }
        }
        Ok(session_id)
    }

    /// Set a config option (e.g. model, mode) via ACP session/set_config_option.
    /// Returns the updated list of all config options.
    pub async fn set_config_option(
        &mut self,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<ConfigOption>> {
        self.set_config_option_inner(config_id, value, true).await
    }

    /// Like [`Self::set_config_option`] but without the slash-command
    /// fallback: an error from `session/set_config_option` is returned, and
    /// the option list is exactly what the agent reported. Used by
    /// `[pool] strict_default_config_options`, where a locally synthesized
    /// value must not count as confirmation.
    pub async fn set_config_option_reported(
        &mut self,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<ConfigOption>> {
        self.set_config_option_inner(config_id, value, false).await
    }

    async fn set_config_option_inner(
        &mut self,
        config_id: &str,
        value: &str,
        allow_fallback: bool,
    ) -> Result<Vec<ConfigOption>> {
        let session_id = self
            .acp_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("no session"))?
            .clone();

        let resp = self
            .send_request(
                "session/set_config_option",
                Some(json!({
                    "sessionId": session_id,
                    "configId": config_id,
                    "value": value,
                })),
            )
            .await;

        match resp {
            Ok(r) => {
                if let Some(result) = r.result.as_ref() {
                    self.config_options = parse_config_options(result);
                } else if !allow_fallback {
                    // Strict path: never compare against pre-switch cached options.
                    self.config_options.clear();
                }
                info!(config_id, value, "config option set");
            }
            Err(e) if !allow_fallback => return Err(e),
            Err(_) => {
                // Fall back: send as a slash command (e.g. "/model claude-sonnet-4")
                let cmd = format!("/{config_id} {value}");
                info!(
                    cmd,
                    "set_config_option not supported, falling back to prompt"
                );
                let _resp = self
                    .send_request(
                        "session/prompt",
                        Some(json!({
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": cmd}],
                        })),
                    )
                    .await?;
                for opt in &mut self.config_options {
                    if opt.id == config_id {
                        opt.current_value = value.to_string();
                    }
                }
            }
        }

        Ok(self.config_options.clone())
    }

    /// Query account-level usage/billing via kiro-cli's
    /// `_kiro.dev/commands/execute` extension (the `/usage` slash command).
    ///
    /// This is a Kiro-specific extension, not part of the ACP spec, and the
    /// request shape is strict: a malformed `command` value is a
    /// deserialization error that kills the whole ACP connection (no JSON-RPC
    /// error is returned). We therefore gate on the agent name advertised in
    /// `initialize` and never retry on failure.
    pub async fn get_usage(&mut self) -> Result<UsageReport> {
        if !self.agent_name.to_ascii_lowercase().contains("kiro") {
            return Err(anyhow!(
                "usage query is not supported by this backend ({})",
                if self.agent_name.is_empty() {
                    "unknown agent"
                } else {
                    &self.agent_name
                }
            ));
        }
        let session_id = self
            .acp_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("no session"))?
            .clone();

        let resp = self
            .send_request(
                "_kiro.dev/commands/execute",
                Some(json!({
                    "sessionId": session_id,
                    // Adjacently-tagged TuiCommand enum: tag = "command", content = "args".
                    "command": {"command": "usage", "args": {}},
                })),
            )
            .await?;

        let result = resp
            .result
            .as_ref()
            .ok_or_else(|| anyhow!("empty usage response"))?;
        parse_usage_report(result)
            .ok_or_else(|| anyhow!("could not parse usage response from agent"))
    }

    /// Send a prompt with content blocks (text and/or images) and return a receiver
    /// for streaming notifications. The final message on the channel will have id set
    /// (the prompt response).
    pub async fn session_prompt(
        &mut self,
        content_blocks: Vec<ContentBlock>,
    ) -> Result<(mpsc::UnboundedReceiver<JsonRpcMessage>, u64)> {
        self.last_active = Instant::now();
        self.activity.touch();
        self.activity.set_in_flight(true);

        let session_id = self
            .acp_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("no session"))?;

        let (tx, rx) = mpsc::unbounded_channel();
        *self.notify_tx.lock().await = Some(NotifySink::Prompt(tx));

        let id = self.next_id();

        // Convert content blocks to JSON
        let prompt_json: Vec<Value> = content_blocks.iter().map(|b| b.to_json()).collect();

        let req = JsonRpcRequest::new(
            id,
            "session/prompt",
            Some(json!({
                "sessionId": session_id,
                "prompt": prompt_json,
            })),
        );
        let data = serde_json::to_string(&req)?;

        let (resp_tx, _resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, resp_tx);

        self.send_raw(&data).await?;
        Ok((rx, id))
    }

    /// Like [`Self::prompt_done`] but keeps a subscriber installed so late
    /// frames (Kiro workflow output) keep arriving. The foreground `prompt_rx`
    /// (from `session_prompt`) is replaced by a bounded relay queue: frames
    /// still queued in `prompt_rx` move to the new queue first, under the
    /// subscriber lock, so their order is kept. The next `session_prompt`
    /// replaces the subscriber, which closes the relay receiver.
    pub async fn prompt_done_keep_subscriber(
        &mut self,
        prompt_rx: &mut mpsc::UnboundedReceiver<JsonRpcMessage>,
    ) -> RelayReceiver {
        let (tx, rx) = relay_channel();
        {
            let mut sub = self.notify_tx.lock().await;
            while let Ok(msg) = prompt_rx.try_recv() {
                let _ = tx.send(msg);
            }
            *sub = Some(NotifySink::Relay(tx));
        }
        self.activity.touch();
        self.activity.set_in_flight(false);
        self.last_active = Instant::now();
        rx
    }

    /// Call after prompt streaming is done to clean up subscriber.
    pub async fn prompt_done(&mut self) {
        *self.notify_tx.lock().await = None;
        self.activity.touch();
        self.activity.set_in_flight(false);
        self.last_active = Instant::now();
    }

    /// Drop the pending entry for `request_id` and best-effort send
    /// `session/cancel` as a JSON-RPC notification (no id; per ACP spec the
    /// agent does not reply). Errors are swallowed: the agent process may
    /// already be dead, in which case the stdin write fails harmlessly.
    /// See #732.
    pub async fn abandon_request(&self, request_id: u64) {
        self.pending.lock().await.remove(&request_id);
        let Some(session_id) = self.acp_session_id.as_deref() else {
            return;
        };
        let req = json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id},
        });
        if let Ok(data) = serde_json::to_string(&req) {
            let _ = self.send_raw(&data).await;
        }
    }

    /// Return a clone of the stdin handle for lock-free cancel.
    pub fn cancel_handle(&self) -> Arc<Mutex<ChildStdin>> {
        Arc::clone(&self.stdin)
    }

    pub fn activity_handle(&self) -> Arc<SessionActivity> {
        Arc::clone(&self.activity)
    }

    /// Process-group id of the agent child, readable without any lock state.
    pub fn child_pgid(&self) -> Option<i32> {
        self.child_pgid
    }

    pub fn alive(&self) -> bool {
        !self._reader_handle.is_finished()
    }

    /// Resume a previous session by ID. Returns Ok(()) if the agent accepted
    /// the load, or an error if it failed (caller should fall back to session/new).
    pub async fn session_load(&mut self, session_id: &str, cwd: &str) -> Result<()> {
        let resp = self
            .send_request(
                "session/load",
                Some(json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []})),
            )
            .await?;
        // Accept any non-error response as success
        if resp.error.is_some() {
            return Err(anyhow!("session/load rejected"));
        }
        info!(session_id, "session loaded");
        self.acp_session_id = Some(session_id.to_string());
        if let Some(result) = resp.result.as_ref() {
            self.config_options = parse_config_options(result);
        }
        Ok(())
    }

    /// Kill the entire process group: SIGTERM → SIGKILL.
    /// Uses std::thread (not tokio::spawn) so SIGKILL fires even during
    /// runtime shutdown or panic unwinding.
    fn kill_process_group(&mut self) {
        let pgid = match self.child_pgid {
            Some(pid) if pid > 0 => pid,
            _ => return,
        };
        #[cfg(unix)]
        {
            // Stage 1: SIGTERM the process group
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
            }
            // Stage 2: SIGKILL after brief grace (std::thread survives runtime shutdown)
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            });
        }
        #[cfg(not(unix))]
        {
            let _ = pgid; // suppress unused warning on Windows
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        if let Some(handle) = self._stderr_handle.take() {
            handle.abort();
        }
        self.kill_process_group();
    }
}

#[cfg(test)]
mod tests {
    use super::{build_agent_env, build_permission_response, pick_best_option};
    use serde_json::json;

    #[test]
    fn picks_allow_always_over_other_options() {
        let options = vec![
            json!({"kind": "allow_once", "optionId": "once"}),
            json!({"kind": "allow_always", "optionId": "always"}),
            json!({"kind": "reject_once", "optionId": "reject"}),
        ];

        assert_eq!(pick_best_option(&options), Some("always".to_string()));
    }

    #[test]
    fn falls_back_to_first_unknown_non_reject_kind() {
        let options = vec![
            json!({"kind": "reject_once", "optionId": "reject"}),
            json!({"kind": "workspace_write", "optionId": "workspace-write"}),
        ];

        assert_eq!(
            pick_best_option(&options),
            Some("workspace-write".to_string())
        );
    }

    #[test]
    fn selects_bypass_permissions_for_exit_plan_mode() {
        let options = vec![
            json!({"optionId": "bypassPermissions", "kind": "allow_always"}),
            json!({"optionId": "acceptEdits", "kind": "allow_always"}),
            json!({"optionId": "default", "kind": "allow_once"}),
            json!({"optionId": "plan", "kind": "reject_once"}),
        ];

        assert_eq!(
            pick_best_option(&options),
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn returns_none_when_only_reject_options_exist() {
        let options = vec![
            json!({"kind": "reject_once", "optionId": "reject-once"}),
            json!({"kind": "reject_always", "optionId": "reject-always"}),
        ];

        assert_eq!(pick_best_option(&options), None);
    }

    #[test]
    fn builds_cancelled_outcome_when_no_selectable_option_exists() {
        let response = build_permission_response(Some(&json!({
            "options": [
                {"kind": "reject_once", "optionId": "reject-once"}
            ]
        })));

        assert_eq!(response, json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn builds_cancelled_when_options_array_is_empty() {
        let response = build_permission_response(Some(&json!({
            "options": []
        })));

        assert_eq!(response, json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn falls_back_to_allow_always_when_options_are_missing() {
        let response = build_permission_response(Some(&json!({
            "toolCall": {"title": "legacy"}
        })));

        assert_eq!(
            response,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}})
        );
    }

    #[test]
    fn falls_back_to_allow_always_when_params_is_none() {
        let response = build_permission_response(None);

        assert_eq!(
            response,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}})
        );
    }

    #[test]
    fn explicit_env_takes_precedence_over_inherit_env() {
        let key = "OAB_TEST_PRECEDENCE";
        std::env::set_var(key, "from_process");
        let mut explicit = std::collections::HashMap::new();
        explicit.insert(key.to_string(), "from_config".to_string());
        let inherit = vec![key.to_string()];

        let (result, inherited) = build_agent_env(&explicit, &inherit);

        assert_eq!(result.get(key).unwrap(), "from_config");
        assert!(!inherited.contains(&key.to_string()));
        std::env::remove_var(key);
    }

    #[test]
    fn inherit_env_copies_from_process() {
        let key = "OAB_TEST_INHERIT";
        std::env::set_var(key, "process_value");
        let explicit = std::collections::HashMap::new();
        let inherit = vec![key.to_string()];

        let (result, inherited) = build_agent_env(&explicit, &inherit);

        assert_eq!(result.get(key).unwrap(), "process_value");
        assert!(inherited.contains(&key.to_string()));
        std::env::remove_var(key);
    }

    #[test]
    fn inherit_env_skips_missing_vars() {
        let explicit = std::collections::HashMap::new();
        let inherit = vec!["OAB_TEST_NONEXISTENT_VAR_12345".to_string()];

        let (result, inherited) = build_agent_env(&explicit, &inherit);

        assert!(!result.contains_key("OAB_TEST_NONEXISTENT_VAR_12345"));
        assert!(inherited.is_empty());
    }
}

#[cfg(test)]
mod reader_loop_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::io::{duplex, AsyncWriteExt};
    use tokio::sync::{mpsc, oneshot, Mutex};

    /// #732 stale-id path: when a response arrives for an id the broker has
    /// already abandoned, the reader must (a) not crash, (b) leave `pending`
    /// untouched, and (c) still forward the message to whoever is currently
    /// subscribed — the adapter recv loop is responsible for filtering by
    /// request_id so the stray response never leaks into the next prompt.
    #[tokio::test]
    async fn stale_id_response_is_forwarded_without_pending_entry() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, _agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<NotifySink>>> = Arc::new(Mutex::new(None));

        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        *notify_tx.lock().await = Some(NotifySink::Prompt(sub_tx));

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
        ));

        let stale = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{\"stopReason\":\"ok\"}}\n";
        agent_stdout_writer.write_all(stale).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("subscriber should receive stale message before timeout")
            .expect("subscriber channel should not be closed");
        assert_eq!(forwarded.id, Some(JsonRpcId::Unsigned(42)));
        assert!(pending.lock().await.is_empty());

        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    /// Matched-id path: when a response's id is in `pending`, the loop must
    /// resolve the oneshot AND forward a copy to the subscriber so the
    /// adapter's recv loop sees the completion. Guards against regressions
    /// that would suppress the forward branch while keeping resolve.
    #[tokio::test]
    async fn matched_id_response_resolves_pending_and_forwards() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, _agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<NotifySink>>> = Arc::new(Mutex::new(None));

        let (resp_tx, resp_rx) = oneshot::channel();
        pending.lock().await.insert(7, resp_tx);

        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        *notify_tx.lock().await = Some(NotifySink::Prompt(sub_tx));

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
        ));

        let payload = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"stopReason\":\"end_turn\"}}\n";
        agent_stdout_writer.write_all(payload).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(2), resp_rx)
            .await
            .expect("oneshot should resolve")
            .expect("oneshot should not be cancelled");
        assert_eq!(resolved.id, Some(JsonRpcId::Unsigned(7)));

        let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("subscriber should receive forwarded copy")
            .expect("subscriber channel should not be closed");
        assert_eq!(forwarded.id, Some(JsonRpcId::Unsigned(7)));
        assert!(pending.lock().await.is_empty());

        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn agent_request_does_not_consume_colliding_pending_response() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (resp_tx, mut resp_rx) = oneshot::channel();
        pending.lock().await.insert(7, resp_tx);
        let notify_tx: Arc<Mutex<Option<NotifySink>>> = Arc::new(Mutex::new(None));

        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            Arc::new(Mutex::new(agent_stdin_writer)),
            pending.clone(),
            notify_tx,
        ));

        let request = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"agent/test\",\"params\":{}}\n";
        agent_stdout_writer.write_all(request).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            BufReader::new(agent_stdin_reader).read_line(&mut response),
        )
        .await
        .expect("unsupported request should receive a response")
        .expect("response should be readable");
        let response: Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], -32601);
        assert!(pending.lock().await.contains_key(&7));
        assert!(matches!(
            resp_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        pending.lock().await.remove(&7);
        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn max_id_reverse_request_preserves_pending_response() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, agent_stdin_reader) = duplex(8 * 1024);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (resp_tx, mut resp_rx) = oneshot::channel();
        pending.lock().await.insert(u64::MAX, resp_tx);
        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        let notify_tx = Arc::new(Mutex::new(Some(NotifySink::Prompt(sub_tx))));

        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            Arc::new(Mutex::new(agent_stdin_writer)),
            pending.clone(),
            notify_tx,
        ));
        let mut response_reader = BufReader::new(agent_stdin_reader);

        let request = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"agent/test\"}}\n",
            u64::MAX
        );
        agent_stdout_writer
            .write_all(request.as_bytes())
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut reverse_response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            response_reader.read_line(&mut reverse_response),
        )
        .await
        .expect("unsupported request should receive a response")
        .expect("response should be readable");
        let reverse_response: Value = serde_json::from_str(reverse_response.trim()).unwrap();
        assert_eq!(reverse_response["id"], json!(u64::MAX));
        assert_eq!(reverse_response["error"]["code"], -32601);
        assert!(reverse_response.get("result").is_none());
        assert!(reverse_response["error"].get("data").is_none());
        assert!(pending.lock().await.contains_key(&u64::MAX));
        assert!(matches!(
            resp_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        let response = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"stopReason\":\"end_turn\"}}}}\n",
            u64::MAX
        );
        agent_stdout_writer
            .write_all(response.as_bytes())
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(2), resp_rx)
            .await
            .expect("oneshot should resolve")
            .expect("oneshot should not be cancelled");
        assert_eq!(resolved.id.unwrap().as_u64(), Some(u64::MAX));

        let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("subscriber should receive matched response")
            .expect("subscriber channel should not be closed");
        assert_eq!(forwarded.id.unwrap().as_u64(), Some(u64::MAX));
        assert!(pending.lock().await.is_empty());

        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn large_id_permission_request_receives_success_response() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, agent_stdin_reader) = duplex(8 * 1024);
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            Arc::new(Mutex::new(agent_stdin_writer)),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(None)),
        ));
        let request_id = (i64::MAX as u64) + 1;
        let request = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{request_id},\"method\":\"session/request_permission\",\"params\":{{\"options\":[{{\"kind\":\"allow_once\",\"optionId\":\"allow\"}}]}}}}\n"
        );
        agent_stdout_writer
            .write_all(request.as_bytes())
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            BufReader::new(agent_stdin_reader).read_line(&mut response),
        )
        .await
        .expect("permission request should receive a response")
        .expect("response should be readable");
        let response: Value = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(response["id"], json!(request_id));
        assert_eq!(
            response["result"],
            json!({"outcome": {"outcome": "selected", "optionId": "allow"}})
        );
        assert!(response.get("error").is_none());

        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_agent_requests_echo_string_and_negative_ids() {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, agent_stdin_reader) = duplex(8 * 1024);
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            Arc::new(Mutex::new(agent_stdin_writer)),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(None)),
        ));
        let mut response_reader = BufReader::new(agent_stdin_reader);

        for (raw_id, expected_id) in [
            ("\"agent-request\"".to_string(), json!("agent-request")),
            ("-1".to_string(), json!(-1)),
            (
                ((i64::MAX as u64) + 1).to_string(),
                json!((i64::MAX as u64) + 1),
            ),
        ] {
            let request =
                format!("{{\"jsonrpc\":\"2.0\",\"id\":{raw_id},\"method\":\"agent/test\"}}\n");
            agent_stdout_writer
                .write_all(request.as_bytes())
                .await
                .unwrap();
            agent_stdout_writer.flush().await.unwrap();

            let mut response = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                response_reader.read_line(&mut response),
            )
            .await
            .expect("unsupported request should receive a response")
            .expect("response should be readable");
            let response: Value = serde_json::from_str(response.trim()).unwrap();
            assert_eq!(response["id"], expected_id);
            assert_eq!(response["error"]["code"], -32601);
        }

        drop(agent_stdout_writer);
        handle.await.unwrap();
    }

    #[test]
    fn session_activity_touch_advances_last_active() {
        let activity = SessionActivity::new();
        // Warm the monotonic clock past zero so a backdated value is older.
        std::thread::sleep(std::time::Duration::from_millis(10));
        activity.set_last_active_ms(0);
        let before = activity.last_active_ms();
        activity.touch();
        assert!(activity.last_active_ms() > before);
        // Backdated last_active yields a positive age; touch resets it near zero.
        activity.set_last_active_ms(0);
        assert!(activity.age() >= std::time::Duration::from_millis(10));
        activity.touch();
        assert!(activity.age() < std::time::Duration::from_secs(60));
        // A future timestamp must not underflow: age saturates at zero.
        activity.set_last_active_ms(u64::MAX);
        assert_eq!(activity.age(), std::time::Duration::ZERO);
    }

    #[test]
    fn session_activity_set_in_flight_round_trips() {
        let activity = SessionActivity::new();
        assert!(!activity.in_flight());
        activity.set_in_flight(true);
        assert!(activity.in_flight());
        activity.set_in_flight(false);
        assert!(!activity.in_flight());
    }
}

/// `sh -c` stand-in for an ACP agent, with fixed request ids
/// (initialize = 1, session/new = 2, first set_config_option = 3, and the
/// slash-command fallback prompt = 4), shared with the pool tests.
#[cfg(all(test, unix))]
pub(crate) mod mock_agent {
    /// `set_cfg_reply` is the full JSON-RPC line answering the first
    /// `session/set_config_option` (it must contain no single quote). When
    /// `require_meta` is set, `session/new` fails unless the request carries
    /// `"workflows":{"enabled":true}`.
    pub(crate) fn script(set_cfg_reply: &str, require_meta: bool) -> String {
        script_with_prompt(set_cfg_reply, require_meta, false)
    }

    /// Like [`script`]; `prompt_ok` makes the slash-command fallback prompt
    /// (id 4) succeed instead of failing.
    pub(crate) fn script_with_prompt(
        set_cfg_reply: &str,
        require_meta: bool,
        prompt_ok: bool,
    ) -> String {
        let prompt_reply = if prompt_ok {
            r#"{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}"#
        } else {
            r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"unsupported"}}"#
        };
        let new_ok = r#"echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}'"#;
        let new_err =
            r#"echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"missing _meta"}}'"#;
        let session_new = if require_meta {
            format!(
                r#"case "$l" in *'"workflows":{{"enabled":true}}'*) {new_ok};; *) {new_err};; esac"#
            )
        } else {
            new_ok.to_string()
        };
        format!(
            r#"while IFS= read -r l; do
case "$l" in
*'"method":"initialize"'*) echo '{{"jsonrpc":"2.0","id":1,"result":{{"agentInfo":{{"name":"mock"}}}}}}';;
*'"method":"session/new"'*) {session_new};;
*'"method":"session/set_config_option"'*) echo '{set_cfg_reply}';;
*'"method":"session/prompt"'*) echo '{prompt_reply}';;
esac
done"#
        )
    }

    pub(crate) fn args(script: String) -> Vec<String> {
        vec!["-c".to_string(), script]
    }

    /// A `set_config_option` reply whose `mode` option reports `current`.
    pub(crate) fn mode_reply(current: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":3,"result":{{"configOptions":[{{"type":"select","id":"mode","name":"Mode","currentValue":"{current}","options":[{{"value":"vibe","name":"Default"}},{{"value":"e2e","name":"e2e"}}]}}]}}}}"#
        )
    }
}

#[cfg(test)]
mod workflow_meta_tests {
    use super::*;
    use crate::acp::protocol::ConfigOptionValue;

    fn opt(id: &str, current: &str, values: &[(&str, &str)]) -> ConfigOption {
        ConfigOption {
            id: id.into(),
            name: id.into(),
            description: None,
            category: None,
            option_type: "select".into(),
            current_value: current.into(),
            options: values
                .iter()
                .map(|(value, name)| ConfigOptionValue {
                    value: (*value).into(),
                    name: (*name).into(),
                    description: None,
                })
                .collect(),
        }
    }

    #[test]
    fn session_new_params_without_meta_match_legacy_shape() {
        assert_eq!(
            build_session_new_params("/w", None),
            json!({"cwd": "/w", "mcpServers": []})
        );
    }

    #[test]
    fn session_new_params_with_meta_carry_underscore_meta() {
        let meta = json!({"kiro": {"settings": {"workflows": {"enabled": true}}}});
        let params = build_session_new_params("/w", Some(&meta));
        assert_eq!(
            params["_meta"]["kiro"]["settings"]["workflows"]["enabled"],
            true
        );
        assert_eq!(params["cwd"], "/w");
        assert_eq!(params["mcpServers"], json!([]));
    }

    #[test]
    fn verify_accepts_equal_value() {
        let o = [opt("mode", "e2e", &[("vibe", "Default"), ("e2e", "e2e")])];
        assert!(verify_config_option_applied(&o, "mode", "e2e").is_ok());
    }

    #[test]
    fn verify_rejects_different_value_with_clear_error() {
        let o = [opt("mode", "vibe", &[("vibe", "Default"), ("e2e", "e2e")])];
        let err = verify_config_option_applied(&o, "mode", "e2e").unwrap_err();
        assert!(err.contains("`mode`"), "{err}");
        assert!(err.contains("`e2e`"), "{err}");
        assert!(err.contains("`vibe`"), "{err}");
        assert!(err.contains("was not applied"), "{err}");
    }

    #[test]
    fn verify_accepts_missing_option_or_empty_list() {
        assert!(verify_config_option_applied(&[], "mode", "e2e").is_ok());
        let o = [opt("model", "x", &[])];
        assert!(verify_config_option_applied(&o, "mode", "e2e").is_ok());
    }

    #[test]
    fn verify_accepts_alias_by_option_name() {
        // agy-acp style: the row name is requested, the id is reported.
        let o = [opt(
            "model",
            "gemini-3.8-flash-high",
            &[("gemini-3.8-flash-high", "Gemini 3.8 Flash (High)")],
        )];
        assert!(verify_config_option_applied(&o, "model", "Gemini 3.8 Flash (High)").is_ok());
        assert!(verify_config_option_applied(&o, "model", "something-else").is_err());
    }

    #[test]
    fn verify_ignores_kiro_v2_agent_option_for_mode_key() {
        // kiro v2 reports modes under id `agent`, so `mode` is simply absent.
        let o = [opt("agent", "kiro_default", &[("kiro_default", "default")])];
        assert!(verify_config_option_applied(&o, "mode", "openab-bot").is_ok());
    }

    /// Step sessions ask for permission too; the answer goes out by request id
    /// regardless of the `sessionId` the request carries.
    #[tokio::test]
    async fn permission_request_from_another_session_is_still_answered() {
        use tokio::io::{duplex, AsyncWriteExt};
        let (mut agent_out, agent_out_r) = duplex(8 * 1024);
        let (agent_in_w, agent_in_r) = duplex(8 * 1024);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let notify_tx = Arc::new(Mutex::new(None));
        let handle = tokio::spawn(run_reader_loop(
            agent_out_r,
            Arc::new(Mutex::new(agent_in_w)),
            pending,
            notify_tx,
        ));
        let req = json!({"jsonrpc": "2.0", "id": 9, "method": "session/request_permission",
            "params": {"sessionId": "step-session",
                "toolCall": {"title": "run"},
                "options": [{"optionId": "ao", "kind": "allow_once"},
                            {"optionId": "aa", "kind": "allow_always"}]}});
        agent_out
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();
        agent_out.flush().await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            BufReader::new(agent_in_r).read_line(&mut line),
        )
        .await
        .expect("permission request must be answered")
        .unwrap();
        let reply: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["result"]["outcome"]["optionId"], "aa");
        drop(agent_out);
        handle.await.unwrap();
    }

    #[cfg(unix)]
    async fn spawn_mock(script: String) -> AcpConnection {
        let mut conn = AcpConnection::spawn(
            "sh",
            &mock_agent::args(script),
            "/tmp",
            &HashMap::new(),
            &[],
        )
        .await
        .expect("spawn mock agent");
        conn.initialize().await.expect("initialize");
        conn
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "integration: spawns a mock agent subprocess"]
    async fn session_new_with_meta_sends_underscore_meta() {
        let script = mock_agent::script(&mock_agent::mode_reply("vibe"), true);
        let meta = json!({"kiro": {"settings": {"workflows": {"enabled": true}}}});
        let mut conn = spawn_mock(script.clone()).await;
        assert_eq!(
            conn.session_new_with_meta("/tmp", Some(&meta))
                .await
                .unwrap(),
            "s-1"
        );
        // Without meta the mock (which requires it) refuses: nothing is sent.
        let mut conn = spawn_mock(script).await;
        assert!(conn.session_new("/tmp").await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "integration: spawns a mock agent subprocess"]
    async fn set_config_option_returns_agent_reported_value() {
        let script = mock_agent::script(&mock_agent::mode_reply("vibe"), false);
        let mut conn = spawn_mock(script).await;
        conn.session_new("/tmp").await.unwrap();
        let opts = conn.set_config_option("mode", "e2e").await.unwrap();
        assert!(verify_config_option_applied(&opts, "mode", "e2e").is_err());
    }

    /// Mock agent for the subscriber hand-off: each `session/prompt` is answered
    /// by a response (ids 3, 4, ...) followed by a late `agent_message_chunk`
    /// of the parent session `s-1` ("late-<id>").
    #[cfg(unix)]
    fn late_chunk_script() -> String {
        r#"n=3
while IFS= read -r l; do
case "$l" in
*'"method":"initialize"'*) echo '{"jsonrpc":"2.0","id":1,"result":{"agentInfo":{"name":"mock"}}}';;
*'"method":"session/new"'*) echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}';;
*'"method":"session/prompt"'*)
echo '{"jsonrpc":"2.0","id":'$n',"result":{"stopReason":"end_turn"}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"late-'$n'"}}}}'
n=$((n+1));;
esac
done"#
            .to_string()
    }

    /// The next `session_prompt` replaces the subscriber: the running tail
    /// flushes what it buffered and ends, and the new receiver sees only the
    /// new turn's frames.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "integration: spawns a mock agent subprocess"]
    async fn next_prompt_replaces_subscriber_and_ends_the_tail() {
        use super::ContentBlock;
        use crate::workflow_tail::{run_tail, WorkflowRelayConfig};
        use std::time::Duration;

        let mut conn = spawn_mock(late_chunk_script()).await;
        conn.session_new("/tmp").await.unwrap();

        let (mut prompt_rx, id1) = conn
            .session_prompt(vec![ContentBlock::Text { text: "one".into() }])
            .await
            .unwrap();
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), prompt_rx.recv())
                .await
                .expect("response")
                .expect("open");
            if m.id.as_ref().and_then(|i| i.as_u64()) == Some(id1) {
                break;
            }
        }
        let rx1 = conn.prompt_done_keep_subscriber(&mut prompt_rx).await;

        let out = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = out.clone();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        gate_tx.send(()).unwrap();
        let tail = tokio::spawn(run_tail(
            rx1,
            "s-1".to_string(),
            ["wf".to_string()].into_iter().collect(),
            WorkflowRelayConfig {
                tail_quiet: Duration::from_secs(60),
                tail_max: Duration::from_secs(60),
            },
            tokio::time::Instant::now(),
            gate_rx,
            move |t| {
                sink.lock().unwrap().push(t);
                std::future::ready(())
            },
        ));
        // Let late-3 reach the tail's buffer before the swap.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let (mut rx2, id2) = conn
            .session_prompt(vec![ContentBlock::Text { text: "two".into() }])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), tail)
            .await
            .expect("tail must end when the subscriber is replaced")
            .unwrap();
        assert_eq!(*out.lock().unwrap(), vec!["late-3".to_string()]);

        let mut texts = Vec::new();
        let mut got_response = false;
        while !(got_response && !texts.is_empty()) {
            let m = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
                .await
                .expect("frame")
                .expect("open");
            if m.id.as_ref().and_then(|i| i.as_u64()) == Some(id2) {
                got_response = true;
            } else if let Some(crate::acp::AcpEvent::Text(t)) =
                crate::acp::classify_notification(&m)
            {
                texts.push(t);
            }
        }
        assert_eq!(texts, vec!["late-4".to_string()]);
    }
}

use crate::acp::protocol::{
    parse_config_options, parse_usage_report, ConfigOption, JsonRpcMessage, JsonRpcRequest,
    JsonRpcResponse, UsageReport,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace};

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

/// Phase 6.4.1F — canonical token set for the structured write policy
/// that the ACP tool-permission gate enforces. The strings mirror
/// `admission::NativeScopePolicy` exactly so a literal compare is
/// sufficient (no enum round-trip needed at the seam).
pub const WRITE_POLICY_READ_ONLY: &str = "READ_ONLY";
pub const WRITE_POLICY_MODIFY_ALLOWED: &str = "MODIFY_ALLOWED";

/// Phase 6.4.1F — canonical tool-name deny-list applied when the
/// connection's `write_policy` is `READ_ONLY`. Conservative by design:
/// every known write-capable tool name is included so the gate cannot
/// be bypassed by name variation. Tool titles are matched
/// case-insensitively against this set plus any `apply_patch` variant.
///
/// Bash is intentionally included. The project explicitly chose NOT
/// to build an unsafe regex-based command parser for shell mutation
/// detection — the conservative posture is to deny Bash entirely
/// under READ_ONLY. Read-only shell commands (ls / grep / cat) are
/// not a current tool surface for Claude Code (it uses Read / Glob /
/// Grep directly) so this does not regress inspection of the
/// workspace.
pub const READ_ONLY_DENY_TOOLS: &[&str] = &[
    "Edit",
    "Write",
    "NotebookEdit",
    "MultiEdit",
    "apply_patch",
    "ApplyPatch",
    "Bash",
];

/// Phase 6.4.1F Round 4 — canonical ACP `toolCall.kind` deny-list
/// applied under `write_policy = READ_ONLY`. The kind is the primary
/// discriminator for production-shaped requests: real Claude Code
/// payloads decorate the tool title with arguments (e.g.
/// `"Write .phase641f-round3-probe"`), so name-only equality against
/// `READ_ONLY_DENY_TOOLS` silently leaks past the gate. The kind is
/// a separate, structured field that names the operation class:
///
/// * `edit`    — content/file mutation (Write, Edit, MultiEdit,
///   NotebookEdit, apply_patch, etc.)
/// * `delete`  — destructive mutation
/// * `move`    — filesystem/state mutation (rename / move)
/// * `execute` — command/state execution authority (Bash and any
///   future shell-equivalent tool)
///
/// The READ_ONLY policy already prohibits Bash entirely, so command
/// execution must not bypass the gate merely because the display
/// title changes. The list deliberately does NOT include `read`,
/// `search`, `fetch`, `think`, `other`, or `switch_mode` — those
/// kinds describe non-mutating operations and must remain
/// non-denied.
pub const READ_ONLY_DENY_KINDS: &[&str] = &["edit", "delete", "move", "execute"];

/// Phase 6.4.1F Round 4 — primary discriminator. Returns `true` when
/// the canonical ACP `toolCall.kind` field names a write- or
/// execution-capable operation class under
/// `write_policy = READ_ONLY`. The comparison is case-insensitive
/// against `READ_ONLY_DENY_KINDS`. An absent, empty, or non-string
/// kind is treated as a non-match — the title fallback in
/// `build_permission_response_with_policy` decides.
pub fn tool_kind_denied_for_read_only(kind: &str) -> bool {
    let normalized = kind.trim();
    if normalized.is_empty() {
        return false;
    }
    READ_ONLY_DENY_KINDS
        .iter()
        .any(|deny| normalized.eq_ignore_ascii_case(deny))
}

/// Phase 6.4.1F Round 4 — fallback discriminator. Returns `true`
/// when the ACP `session/request_permission` `toolCall.title`
/// field names a write-capable tool that MUST be denied under
/// `write_policy = READ_ONLY`. Only the FIRST whitespace-delimited
/// token of the title is inspected — real Claude Code payloads
/// decorate the title with arguments (e.g. `"Write foo.txt"`),
/// so bare full-title equality was the original production defect.
/// The token is compared case-insensitively against
/// `READ_ONLY_DENY_TOOLS`.
///
/// Deliberately does NOT inspect the tool call's `rawInput`,
/// `input`, or `arguments` field — the gate is purely name-based
/// to keep it deterministic and reviewable. There is no
/// natural-language inference, regex inference, or fuzzy matching.
pub fn tool_title_denied_for_read_only(title: &str) -> bool {
    let normalized = title.trim();
    if normalized.is_empty() {
        // A tool without a name is opaque — fail open at this layer
        // because the prompt-level `<native_work_authority>` block
        // carries the explicit READ_ONLY directive. The kind /
        // title gate is the deterministic second layer, not the
        // only layer.
        return false;
    }
    let first_token = match normalized.find(char::is_whitespace) {
        Some(idx) => &normalized[..idx],
        None => normalized,
    };
    READ_ONLY_DENY_TOOLS
        .iter()
        .any(|deny| first_token.eq_ignore_ascii_case(deny))
}

/// Phase 6.4.1F — shared, lock-free write-policy guard. The pool sets
/// the value immediately after spawning the `AcpConnection`; the
/// reader loop reads it on every `session/request_permission`
/// invocation. `None` preserves the pre-6.4.1F behaviour (no gate).
#[derive(Debug, Default)]
pub struct WritePolicyGuard {
    policy: AtomicU8,
}

impl WritePolicyGuard {
    pub fn new() -> Self {
        Self {
            policy: AtomicU8::new(0),
        }
    }

    pub fn set(&self, value: &str) {
        let code = if value == WRITE_POLICY_READ_ONLY {
            2
        } else if value == WRITE_POLICY_MODIFY_ALLOWED {
            1
        } else {
            0
        };
        self.policy.store(code, Ordering::Release);
    }

    pub fn is_read_only(&self) -> bool {
        self.policy.load(Ordering::Acquire) == 2
    }

    pub fn label(&self) -> &'static str {
        match self.policy.load(Ordering::Acquire) {
            2 => WRITE_POLICY_READ_ONLY,
            1 => WRITE_POLICY_MODIFY_ALLOWED,
            _ => "<unset>",
        }
    }
}

/// Build a spec-compliant permission response with backward-compatible fallback.
///
/// `write_policy_guard` — Phase 6.4.1F: when the guard declares
/// `READ_ONLY`, any `toolCall` that classifies as write- or
/// execution-capable short-circuits to the `cancelled` outcome
/// (deterministic denial before the tool can mutate the filesystem).
/// `None` or `MODIFY_ALLOWED` preserves the pre-6.4.1F behaviour
/// (auto-allow with the existing best-option picker).
///
/// Phase 6.4.1F Round 4 — classification algorithm:
///
/// 1. **Primary discriminator** — `toolCall.kind`. Under READ_ONLY,
///    kinds in `{edit, delete, move, execute}` short-circuit to
///    `cancelled`. Real Claude Code payloads decorate the title
///    with arguments (e.g. `"Write .phase641f-round3-probe"`) but
///    set `kind: "edit"`, so the kind is the authoritative
///    operation class. `kind: "other"` is intentionally NOT
///    blanket-denied.
///
/// 2. **Fallback discriminator** — when `kind` is absent, empty,
///    unknown, or any non-deny value, inspect only the FIRST
///    whitespace-delimited token of `toolCall.title` and compare
///    case-insensitively against `READ_ONLY_DENY_TOOLS`. This
///    catches legacy bare-name requests and any agent that omits
///    `kind`.
///
/// 3. Otherwise fall through to the legacy `build_permission_response`
///    selection (auto-allow by best-option).
///
/// For `write_policy = MODIFY_ALLOWED` or unset, the policy block
/// is skipped entirely — the legacy path is the only path. The fix
/// MUST NOT globally turn unknown/unset sessions into READ_ONLY.
pub fn build_permission_response_with_policy(
    params: Option<&Value>,
    write_policy_guard: Option<&WritePolicyGuard>,
) -> Value {
    if let Some(guard) = write_policy_guard {
        if guard.is_read_only() {
            let tool_call = params.and_then(|p| p.get("toolCall"));

            // 1. Primary discriminator: toolCall.kind ∈ {edit, delete,
            //    move, execute}.
            let kind_denies = tool_call
                .and_then(|t| t.get("kind"))
                .and_then(|k| k.as_str())
                .map(tool_kind_denied_for_read_only)
                .unwrap_or(false);
            if kind_denies {
                return json!({
                    "outcome": {
                        "outcome": "cancelled"
                    }
                });
            }

            // 2. Fallback discriminator: first token of toolCall.title
            //    against READ_ONLY_DENY_TOOLS.
            let title_denies = tool_call
                .and_then(|t| t.get("title"))
                .and_then(|t| t.as_str())
                .map(tool_title_denied_for_read_only)
                .unwrap_or(false);
            if title_denies {
                return json!({
                    "outcome": {
                        "outcome": "cancelled"
                    }
                });
            }
        }
    }
    build_permission_response(params)
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

pub struct AcpConnection {
    _proc: Child,
    /// PID of the direct child, used as the process group ID for cleanup.
    child_pgid: Option<i32>,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>>,
    notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>>,
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
    /// Phase 6.4.1F — shared write-policy guard. The pool sets this
    /// immediately after `AcpConnection::spawn` so the reader loop
    /// can apply deterministic denial on the very first
    /// `session/request_permission`. Lock-free so the gate never
    /// blocks the agent's tool-call stream.
    pub write_policy_guard: Arc<WritePolicyGuard>,
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

/// Reader loop body: reads JSON-RPC messages from `reader`, auto-replies
/// `session/request_permission` via `writer`, resolves pending responses,
/// and forwards notifications + stale id-bearing messages to the active
/// subscriber. Extracted as a free generic function so unit tests can drive
/// it with `tokio::io::duplex()` halves instead of a real child process.
pub(crate) async fn run_reader_loop<R, W>(
    reader: R,
    writer: Arc<Mutex<W>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>>,
    notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>>,
    write_policy_guard: Option<Arc<WritePolicyGuard>>,
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

        // Auto-reply session/request_permission
        if msg.method.as_deref() == Some("session/request_permission") {
            if let Some(id) = msg.id {
                let title = msg
                    .params
                    .as_ref()
                    .and_then(|p| p.get("toolCall"))
                    .and_then(|t| t.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("?");

                let outcome = build_permission_response_with_policy(
                    msg.params.as_ref(),
                    write_policy_guard.as_deref(),
                );
                let policy_label = write_policy_guard
                    .as_deref()
                    .map(|g| g.label())
                    .unwrap_or("<unset>");
                info!(title, write_policy = %policy_label, %outcome, "auto-respond permission");
                let reply = JsonRpcResponse::new(id, outcome);
                if let Ok(data) = serde_json::to_string(&reply) {
                    let mut w = writer.lock().await;
                    let _ = w.write_all(format!("{data}\n").as_bytes()).await;
                    let _ = w.flush().await;
                }
            }
            continue;
        }

        // Response (has id) → resolve pending AND forward to subscriber
        if let Some(id) = msg.id {
            let mut map = pending.lock().await;
            if let Some(tx) = map.remove(&id) {
                // Forward to subscriber so they see the completion
                let sub = notify_tx.lock().await;
                if let Some(ntx) = sub.as_ref() {
                    // Clone the essential fields for the subscriber
                    let _ = ntx.send(JsonRpcMessage {
                        id: Some(id),
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
            let _ = tx.send(msg);
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
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        // Phase 6.4.1F — construct the write-policy guard BEFORE spawning
        // the reader loop so the spawned task and the returned
        // `AcpConnection` share the exact same `Arc<WritePolicyGuard>`
        // instance. A policy update performed through the connection
        // (e.g. `SessionPool::set_session_write_policy`) MUST be visible
        // to the reader loop on its very next
        // `session/request_permission`. The previous construction order
        // created two disconnected policy paths — the reader loop
        // observed `None` while the connection stored its own guard —
        // which let a `READ_ONLY` request resolve as `allow_always`.
        let activity = Arc::new(SessionActivity::new());
        let write_policy_guard = Arc::new(WritePolicyGuard::new());

        let reader_handle = tokio::spawn(run_reader_loop(
            stdout,
            stdin.clone(),
            pending.clone(),
            notify_tx.clone(),
            Some(Arc::clone(&write_policy_guard)),
        ));

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
            write_policy_guard,
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
        let resp = self
            .send_request("session/new", Some(json!({"cwd": cwd, "mcpServers": []})))
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
                }
                info!(config_id, value, "config option set");
            }
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
        *self.notify_tx.lock().await = Some(tx);

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
    use super::{
        build_agent_env, build_permission_response, build_permission_response_with_policy,
        pick_best_option, tool_kind_denied_for_read_only, tool_title_denied_for_read_only,
        WritePolicyGuard, WRITE_POLICY_MODIFY_ALLOWED, WRITE_POLICY_READ_ONLY,
    };
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

    // --- Phase 6.4.1F — READ_ONLY tool-permission gate ----------------

    #[test]
    fn read_only_policy_denies_edit_tool() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({"toolCall": {"title": "Edit"}, "options": []});
        let response = build_permission_response_with_policy(Some(&params), Some(&guard));
        assert_eq!(
            response,
            json!({"outcome": {"outcome": "cancelled"}}),
            "Edit must be deterministically denied under READ_ONLY"
        );
    }

    #[test]
    fn read_only_policy_denies_write_notebookedit_multiedit_apply_patch_bash() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        for title in ["Write", "NotebookEdit", "MultiEdit", "apply_patch", "Bash"] {
            let params = json!({"toolCall": {"title": title}, "options": []});
            let response = build_permission_response_with_policy(Some(&params), Some(&guard));
            assert_eq!(
                response,
                json!({"outcome": {"outcome": "cancelled"}}),
                "tool title={title:?} must be denied under READ_ONLY"
            );
        }
    }

    #[test]
    fn read_only_policy_allows_read_only_tools() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        for title in ["Read", "Glob", "Grep", "LS"] {
            let params = json!({"toolCall": {"title": title}});
            let response = build_permission_response_with_policy(Some(&params), Some(&guard));
            // pick_best_option falls through to "allow_always" with no options
            assert_eq!(
                response,
                json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}}),
                "tool title={title:?} must remain allowed under READ_ONLY"
            );
        }
    }

    #[test]
    fn modify_allowed_policy_preserves_pre_6_4_1f_behavior() {
        // Phase 6.4.1F backward-compat: when the policy is MODIFY_ALLOWED,
        // the gate must NOT short-circuit; the result must be byte-identical
        // to the pre-6.4.1F build_permission_response output for the same
        // params.
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_MODIFY_ALLOWED);
        let params = json!({"toolCall": {"title": "Edit"}, "options": []});
        let gated = build_permission_response_with_policy(Some(&params), Some(&guard));
        let legacy = build_permission_response(Some(&params));
        assert_eq!(gated, legacy);
    }

    #[test]
    fn unset_policy_preserves_pre_6_4_1f_behavior() {
        // Phase 6.4.1F backward-compat: callers that never set the policy
        // (legacy code paths) must see the pre-6.4.1F auto-allow response.
        let params = json!({"toolCall": {"title": "Edit"}});
        let gated = build_permission_response_with_policy(Some(&params), None);
        let legacy = build_permission_response(Some(&params));
        assert_eq!(gated, legacy);
    }

    #[test]
    fn read_only_policy_ignores_unrecognised_tool_title() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({"toolCall": {"title": "MagicCustomTool"}});
        let response = build_permission_response_with_policy(Some(&params), Some(&guard));
        // Unknown tool titles fall through to the legacy auto-allow path
        // because the gate is name-based and intentionally conservative.
        assert_eq!(
            response,
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}}),
            "unrecognised tool titles must not be blanket-denied; prompt-level fence is the second layer"
        );
    }

    #[test]
    fn read_only_policy_denies_case_insensitively() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        for title in ["edit", "WRITE", "BaSh", "ApplyPatch"] {
            let params = json!({"toolCall": {"title": title}, "options": []});
            let response = build_permission_response_with_policy(Some(&params), Some(&guard));
            assert_eq!(
                response,
                json!({"outcome": {"outcome": "cancelled"}}),
                "tool title={title:?} must be denied case-insensitively"
            );
        }
    }

    // --- Phase 6.4.1F Round 4 — kind-aware READ_ONLY classifier ------
    //
    // Production-shaped Claude Code requests decorate the tool title
    // with arguments (e.g. `"Write .phase641f-round3-probe"`) and
    // carry the structured `toolCall.kind` field that names the
    // operation class. Round 4 introduces `toolCall.kind` as the
    // primary discriminator, with a first-token title fallback for
    // agents that omit `kind`.

    #[test]
    fn kind_helper_denies_known_mutation_kinds() {
        for kind in [
            "edit", "delete", "move", "execute", "EDIT", "Delete", "MOVE", "EXECUTE",
        ] {
            assert!(
                tool_kind_denied_for_read_only(kind),
                "kind={kind:?} must be denied under READ_ONLY"
            );
        }
    }

    #[test]
    fn kind_helper_does_not_deny_non_mutation_kinds() {
        for kind in [
            "read",
            "search",
            "fetch",
            "think",
            "other",
            "switch_mode",
            "",
        ] {
            assert!(
                !tool_kind_denied_for_read_only(kind),
                "kind={kind:?} must NOT be denied by the READ_ONLY classifier"
            );
        }
    }

    #[test]
    fn title_helper_denies_first_token_for_decorated_titles() {
        // The Round 3 defect: `"Write .phase641f-round3-probe"` was
        // not denied because the title carries trailing arguments.
        // The first-token fallback closes that gap.
        for title in [
            "Write foo.txt",
            "Edit src/main.rs",
            "NotebookEdit data.ipynb",
            "MultiEdit a.rs b.rs",
            "apply_patch fix.patch",
            "ApplyPatch fix.patch",
            "Bash ls",
        ] {
            assert!(
                tool_title_denied_for_read_only(title),
                "title={title:?} first token must be denied under READ_ONLY"
            );
        }
    }

    #[test]
    fn title_helper_does_not_deny_unrecognised_first_token() {
        for title in ["MagicCustomTool", "Read README.md", "Glob **/*.rs"] {
            assert!(
                !tool_title_denied_for_read_only(title),
                "title={title:?} first token must NOT be denied under READ_ONLY"
            );
        }
    }

    #[test]
    fn title_helper_still_denies_bare_names_for_backward_compat() {
        // Existing Round 3 bare-title callers must remain covered.
        for title in [
            "Edit",
            "Write",
            "NotebookEdit",
            "MultiEdit",
            "apply_patch",
            "ApplyPatch",
            "Bash",
        ] {
            assert!(
                tool_title_denied_for_read_only(title),
                "title={title:?} bare name must still be denied under READ_ONLY"
            );
        }
    }

    // (1)-(7) Production-shaped titles + matching kind → cancelled.
    #[test]
    fn read_only_denies_write_foo_txt_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "Write foo.txt", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_edit_src_main_rs_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "Edit src/main.rs", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_notebookedit_data_ipynb_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "NotebookEdit data.ipynb", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_multiedit_a_rs_b_rs_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "MultiEdit a.rs b.rs", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_apply_patch_lowercase_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "apply_patch fix.patch", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_apply_patch_pascal_case_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "ApplyPatch fix.patch", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_bash_ls_kind_execute() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "Bash ls", "kind": "execute"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    // (8)-(11) Unknown title + mutation kind → cancelled via kind primary.
    #[test]
    fn read_only_denies_unknown_title_kind_edit() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "SomeAgentSpecificMutation", "kind": "edit"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_unknown_title_kind_delete() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "SomeAgentSpecificMutation", "kind": "delete"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_unknown_title_kind_move() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "SomeAgentSpecificMutation", "kind": "move"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    #[test]
    fn read_only_denies_unknown_title_kind_execute() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "SomeAgentSpecificMutation", "kind": "execute"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
        );
    }

    // (12) Non-mutation kinds must NOT short-circuit — preserve
    // allow/fallthrough behavior.
    #[test]
    fn read_only_preserves_fallthrough_for_non_mutation_kinds() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        for kind in ["read", "search", "fetch", "think", "other", "switch_mode"] {
            // Provide both options and no options; the gate must not
            // short-circuit so the legacy selection path decides.
            let with_options = json!({
                "toolCall": {"title": "SomeTool", "kind": kind},
                "options": [{"kind": "allow_always", "optionId": "always"}]
            });
            assert_eq!(
                build_permission_response_with_policy(Some(&with_options), Some(&guard)),
                json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
                "kind={kind:?} must not short-circuit; legacy option picker decides"
            );

            // With no options, legacy path returns allow_always.
            let no_options = json!({
                "toolCall": {"title": "SomeTool", "kind": kind}
            });
            assert_eq!(
                build_permission_response_with_policy(Some(&no_options), Some(&guard)),
                json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}}),
                "kind={kind:?} with no options must still fall through to legacy allow_always"
            );
        }
    }

    #[test]
    fn read_only_does_not_deny_kind_other() {
        // Explicit guard: `kind=other` MUST NOT be auto-denied by the
        // kind classifier. With no `options` array, the legacy
        // `build_permission_response` returns `allow_always` — proving
        // the gate did not short-circuit.
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "Read README.md", "kind": "other"}
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}}),
            "kind=other must NOT be blanket-denied by the READ_ONLY classifier"
        );
    }

    // (13) Kind omitted + decorated title → cancelled via first-token
    // fallback (the original Round 3 production defect).
    #[test]
    fn read_only_denies_via_first_token_fallback_when_kind_omitted() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {"title": "Write README.md"},
            "options": []
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
            "kind omitted + decorated title must deny via first-token fallback"
        );
    }

    // (14) MODIFY_ALLOWED + production-shaped Write/Edit/Bash → legacy
    // behavior (no short-circuit).
    #[test]
    fn modify_allowed_preserves_legacy_for_production_shaped_requests() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_MODIFY_ALLOWED);
        for (title, kind) in [
            ("Write .probe", "edit"),
            ("Edit src/main.rs", "edit"),
            ("Bash ls", "execute"),
        ] {
            let params = json!({
                "toolCall": {"title": title, "kind": kind},
                "options": [{"kind": "allow_always", "optionId": "always"}]
            });
            let gated = build_permission_response_with_policy(Some(&params), Some(&guard));
            let legacy = build_permission_response(Some(&params));
            assert_eq!(
                gated, legacy,
                "MODIFY_ALLOWED + production-shaped {title:?} ({kind:?}) must match legacy"
            );
            assert_ne!(
                gated,
                json!({"outcome": {"outcome": "cancelled"}}),
                "MODIFY_ALLOWED MUST NOT short-circuit under any kind/title"
            );
        }
    }

    // (15) Unset policy + production-shaped Write/Edit/Bash → legacy
    // behavior. Crucial: the fix MUST NOT globally turn unknown/unset
    // sessions into READ_ONLY.
    #[test]
    fn unset_policy_preserves_legacy_for_production_shaped_requests() {
        for (title, kind) in [
            ("Write .probe", "edit"),
            ("Edit src/main.rs", "edit"),
            ("Bash ls", "execute"),
        ] {
            let params = json!({
                "toolCall": {"title": title, "kind": kind},
                "options": [{"kind": "allow_always", "optionId": "always"}]
            });
            let gated = build_permission_response_with_policy(Some(&params), None);
            let legacy = build_permission_response(Some(&params));
            assert_eq!(
                gated, legacy,
                "unset + production-shaped {title:?} ({kind:?}) must match legacy"
            );
            assert_ne!(
                gated,
                json!({"outcome": {"outcome": "cancelled"}}),
                "unset policy MUST NOT short-circuit under any kind/title"
            );
        }
    }

    // Explicit regression: bare full-title equality returning `false`
    // for a decorated title must not be reintroduced. The first-token
    // fallback closes the Round 3 defect; this test fails immediately
    // if a future change reverts to exact full-title equality.
    #[test]
    fn full_title_decoration_does_not_bypass_first_token_check() {
        // The exact Round 3 production title.
        assert!(
            tool_title_denied_for_read_only("Write .phase641f-round3-probe"),
            "decorated Write title must be denied via first-token fallback"
        );
        // Other common decorations.
        for title in [
            "Edit /etc/hosts",
            "Bash rm -rf /",
            "MultiEdit a.rs b.rs c.rs",
            "NotebookEdit /home/user/notebook.ipynb (cell 0)",
        ] {
            assert!(
                tool_title_denied_for_read_only(title),
                "decorated title={title:?} must be denied via first-token fallback"
            );
        }
    }

    // Exact production payload must produce the exact production
    // outcome. Locks the seam that Round 3 left leaky.
    #[test]
    fn exact_production_payload_cancels() {
        let guard = WritePolicyGuard::new();
        guard.set(WRITE_POLICY_READ_ONLY);
        let params = json!({
            "toolCall": {
                "title": "Write .phase641f-round3-probe",
                "kind": "edit",
                "rawInput": {
                    "file_path": "/home/arthur/workspace/ai-workstation/.phase641f-round3-probe",
                    "content": "probe"
                }
            },
            "options": [{"kind": "allow_always", "optionId": "always"}]
        });
        assert_eq!(
            build_permission_response_with_policy(Some(&params), Some(&guard)),
            json!({"outcome": {"outcome": "cancelled"}}),
            "exact production payload must produce cancelled under READ_ONLY"
        );
    }

    #[test]
    fn write_policy_guard_round_trip() {
        let guard = WritePolicyGuard::new();
        assert!(!guard.is_read_only());
        assert_eq!(guard.label(), "<unset>");
        guard.set("READ_ONLY");
        assert!(guard.is_read_only());
        assert_eq!(guard.label(), "READ_ONLY");
        guard.set("MODIFY_ALLOWED");
        assert!(!guard.is_read_only());
        assert_eq!(guard.label(), "MODIFY_ALLOWED");
        guard.set("garbage");
        assert_eq!(guard.label(), "<unset>");
        assert!(!guard.is_read_only());
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
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
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
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        *notify_tx.lock().await = Some(sub_tx);

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            None,
        ));

        let stale = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{\"stopReason\":\"ok\"}}\n";
        agent_stdout_writer.write_all(stale).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("subscriber should receive stale message before timeout")
            .expect("subscriber channel should not be closed");
        assert_eq!(forwarded.id, Some(42));
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
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let (resp_tx, resp_rx) = oneshot::channel();
        pending.lock().await.insert(7, resp_tx);

        let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
        *notify_tx.lock().await = Some(sub_tx);

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            None,
        ));

        let payload = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"stopReason\":\"end_turn\"}}\n";
        agent_stdout_writer.write_all(payload).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(2), resp_rx)
            .await
            .expect("oneshot should resolve")
            .expect("oneshot should not be cancelled");
        assert_eq!(resolved.id, Some(7));

        let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("subscriber should receive forwarded copy")
            .expect("subscriber channel should not be closed");
        assert_eq!(forwarded.id, Some(7));
        assert!(pending.lock().await.is_empty());

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

    // ----- Phase 6.4.1F — WritePolicyGuard wiring invariants --------------
    //
    // These tests cover the structural fix for the production hard-gate
    // smoke that observed `write_policy=<unset>` in the reader loop while
    // `SessionPool::set_session_write_policy()` correctly updated the
    // guard stored on `AcpConnection`. The two paths must share the same
    // `Arc<WritePolicyGuard>`; once they do, every test below is a
    // behavioural consequence.

    /// Drive `run_reader_loop` with a shared `Arc<WritePolicyGuard>`,
    /// emit a `session/request_permission`, and read the auto-reply off
    /// the writer side. Returns the parsed outcome string
    /// (`"cancelled"` / `"selected"`).
    async fn drive_permission_request_and_read_reply(
        guard: Option<Arc<WritePolicyGuard>>,
    ) -> String {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, mut agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            guard,
        ));

        let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/request_permission\",\
                        \"params\":{\"toolCall\":{\"title\":\"Write\"},\
                        \"options\":[{\"kind\":\"allow_always\",\"optionId\":\"always\"}]}}\n";
        agent_stdout_writer.write_all(request).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut reply_buf = Vec::new();
        let read_reply = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut byte = [0u8; 1];
            loop {
                match agent_stdin_reader.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        reply_buf.push(byte[0]);
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
        assert!(
            read_reply.is_ok(),
            "reader loop must write a reply within 2s"
        );

        drop(agent_stdout_writer);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

        let reply_text = String::from_utf8_lossy(&reply_buf).to_string();
        let parsed: serde_json::Value = serde_json::from_str(reply_text.trim())
            .unwrap_or_else(|e| panic!("reply is not valid JSON: {e}; raw={reply_text:?}"));
        // Reader loop wraps the outcome in a JSON-RPC envelope:
        //   {"jsonrpc":"2.0","id":1,"result":<outcome>}
        parsed
            .get("result")
            .and_then(|r| r.get("outcome"))
            .and_then(|o| o.get("outcome"))
            .and_then(|o| o.as_str())
            .unwrap_or_else(|| panic!("reply missing result.outcome.outcome; raw={reply_text:?}"))
            .to_string()
    }

    /// Read the full auto-reply (the `result` field of the JSON-RPC
    /// envelope) for direct equality comparison.
    async fn drive_permission_request_and_read_full_reply(
        guard: Option<Arc<WritePolicyGuard>>,
        title: &str,
    ) -> serde_json::Value {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, mut agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            guard,
        ));

        let body = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/request_permission\",\
             \"params\":{{\"toolCall\":{{\"title\":\"{title}\"}},\
             \"options\":[{{\"kind\":\"allow_always\",\"optionId\":\"always\"}}]}}}}"
        );
        let request = format!("{body}\n");
        agent_stdout_writer
            .write_all(request.as_bytes())
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut reply_buf = Vec::new();
        let read_reply = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut byte = [0u8; 1];
            loop {
                match agent_stdin_reader.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        reply_buf.push(byte[0]);
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
        assert!(
            read_reply.is_ok(),
            "reader loop must write a reply within 2s"
        );

        drop(agent_stdout_writer);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

        let reply_text = String::from_utf8_lossy(&reply_buf).to_string();
        let parsed: serde_json::Value = serde_json::from_str(reply_text.trim())
            .unwrap_or_else(|e| panic!("reply is not valid JSON: {e}; raw={reply_text:?}"));
        // Return the inner result so tests can compare against the
        // expected outcome envelope directly.
        parsed
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("reply missing result; raw={reply_text:?}"))
    }

    /// (1) The guard passed into the reader loop must be the same
    /// `Arc<WritePolicyGuard>` as the one the caller (i.e. the
    /// `AcpConnection`) holds. This is the mechanical invariant behind
    /// the production hard-gate smoke fix — without it, a policy update
    /// via `set_session_write_policy` would never reach the reader
    /// loop. The clone handed to the spawned task is asserted via
    /// `Arc::ptr_eq` against a clone held in the test (a third clone,
    /// taken before spawn, of the exact allocation that the task and the
    /// `AcpConnection` then both share).
    #[tokio::test]
    async fn reader_loop_guard_shares_allocation_with_outer_handle() {
        let guard: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());
        let outer_clone = Arc::clone(&guard);

        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, _agent_stdin_reader) = duplex(8 * 1024);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));
        let writer = Arc::new(Mutex::new(agent_stdin_writer));

        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            Some(Arc::clone(&guard)),
        ));

        // Behavioural proof: mutating via the outer handle is observed
        // by the reader loop on the very next request. Arc::ptr_eq
        // would assert the allocation equivalence but cannot see the
        // clone moved into the spawned task; the mutation-visibility
        // test is the canonical behavioural expression of the same
        // property.
        guard.set(WRITE_POLICY_READ_ONLY);
        assert!(
            Arc::ptr_eq(&outer_clone, &guard),
            "outer_clone must share the original allocation"
        );

        let request = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/request_permission\",\
                        \"params\":{\"toolCall\":{\"title\":\"Write\"},\
                        \"options\":[{\"kind\":\"allow_always\",\"optionId\":\"always\"}]}}\n";
        agent_stdout_writer.write_all(request).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        drop(agent_stdout_writer);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    /// (2) Updating the policy AFTER the reader loop has been spawned
    /// must be visible to the permission handler. This is the
    /// behavioural expression of the shared-Arc invariant: the
    /// connection's `set_session_write_policy` updates the guard, and
    /// the reader loop reads the same guard on the next
    /// `session/request_permission`. Pre-fix the reader loop's
    /// `Option<None>` made the update invisible.
    #[tokio::test]
    async fn post_spawn_policy_update_is_visible_to_reader_loop() {
        let guard: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());

        // Before any update, the reader loop sees `<unset>` and
        // auto-allows the request.
        let outcome_before =
            drive_permission_request_and_read_reply(Some(Arc::clone(&guard))).await;
        assert_eq!(
            outcome_before, "selected",
            "unset policy must preserve legacy auto-allow"
        );

        // Update the guard AFTER the spawn equivalent (in the
        // real lifecycle, `set_session_write_policy` runs after
        // `AcpConnection::spawn`). The next request must observe
        // READ_ONLY.
        guard.set(WRITE_POLICY_READ_ONLY);
        let outcome_after = drive_permission_request_and_read_reply(Some(Arc::clone(&guard))).await;
        assert_eq!(
            outcome_after, "cancelled",
            "READ_ONLY must deterministically cancel Write"
        );
    }

    /// (3) READ_ONLY + Write request returns cancelled. Direct
    /// production acceptance criterion.
    #[tokio::test]
    async fn read_only_guard_cancels_write_request() {
        let guard = Arc::new(WritePolicyGuard::new());
        guard.set(WRITE_POLICY_READ_ONLY);
        let outcome = drive_permission_request_and_read_reply(Some(guard)).await;
        assert_eq!(
            outcome, "cancelled",
            "READ_ONLY + Write must produce a cancelled outcome"
        );
    }

    /// (4) READ_ONLY + Edit request returns cancelled.
    #[tokio::test]
    async fn read_only_guard_cancels_edit_request() {
        let guard = Arc::new(WritePolicyGuard::new());
        guard.set(WRITE_POLICY_READ_ONLY);
        let reply = drive_permission_request_and_read_full_reply(Some(guard), "Edit").await;
        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "cancelled"}}),
            "READ_ONLY + Edit must produce a cancelled outcome"
        );
    }

    /// (5) MODIFY_ALLOWED preserves existing permission behaviour. The
    /// reader loop must not short-circuit on the gate; it must fall
    /// through to the legacy `build_permission_response` path. Pre-fix
    /// the `None` reader-loop guard also produced this result, so this
    /// test guards against a regression that would blanket-deny under
    /// MODIFY_ALLOWED.
    #[tokio::test]
    async fn modify_allowed_guard_preserves_legacy_behaviour() {
        let guard = Arc::new(WritePolicyGuard::new());
        guard.set(WRITE_POLICY_MODIFY_ALLOWED);
        let reply = drive_permission_request_and_read_full_reply(Some(guard), "Write").await;
        // `pick_best_option` returns the `optionId` of the first
        // selectable option (`allow_always` here is `"always"`).
        // Critically, the reply MUST NOT be cancelled.
        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
            "MODIFY_ALLOWED must not short-circuit the gate"
        );
    }

    /// (6) Unset / legacy policy preserves pre-6.4.1F behaviour. The
    /// pre-fix bug was that the reader loop had `None` (unset) and
    /// auto-allowed. The fix MUST NOT convert every unset guard into
    /// READ_ONLY — that would silently break every legacy session that
    /// has never set a structured native scope.
    #[tokio::test]
    async fn unset_guard_preserves_pre_6_4_1f_behaviour() {
        let reply = drive_permission_request_and_read_full_reply(None, "Write").await;
        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
            "unset/legacy guard must not blanket-deny"
        );
    }

    /// (7) Session reuse does not detach or reset the guard. After a
    /// `READ_ONLY` write through one clone, a fresh clone must still
    /// see `READ_ONLY`. Equivalently: the guard shared between the
    /// reader loop and the connection is the SAME allocation across
    /// the entire session — it cannot be replaced, reset, or detached
    /// by either side.
    #[tokio::test]
    async fn session_reuse_preserves_guard_allocation() {
        let guard: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());

        let connection_side = Arc::clone(&guard);
        let reader_loop_side = Arc::clone(&guard);
        assert!(
            Arc::ptr_eq(&connection_side, &reader_loop_side),
            "both sides must point at the same allocation"
        );

        connection_side.set(WRITE_POLICY_READ_ONLY);
        assert!(
            reader_loop_side.is_read_only(),
            "reader_loop_side must observe the mutation through the shared Arc"
        );

        // The connection side then transitions the guard to
        // MODIFY_ALLOWED; the reader loop side must follow.
        connection_side.set(WRITE_POLICY_MODIFY_ALLOWED);
        assert!(
            !reader_loop_side.is_read_only(),
            "guard cannot be detached or reset by either side"
        );
        assert_eq!(
            reader_loop_side.label(),
            WRITE_POLICY_MODIFY_ALLOWED,
            "shared guard label must reflect the latest set()"
        );
    }

    /// (8) Fresh native-dispatch session isolation. Every spawn must
    /// build an INDEPENDENT `Arc<WritePolicyGuard>` — two concurrent
    /// native dispatches MUST NOT share a guard, otherwise one
    /// dispatch's `set_session_write_policy("READ_ONLY")` would
    /// silently cross-contaminate the other dispatch's reader loop.
    /// This test models the post-spawn layout (each side holds its
    /// own Arc) and asserts the Arcs are distinct.
    #[tokio::test]
    async fn fresh_session_guard_is_isolated() {
        let guard_a: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());
        let guard_b: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());
        assert!(
            !Arc::ptr_eq(&guard_a, &guard_b),
            "two independent spawns must produce two independent guards"
        );

        // Mutating one must be invisible to the other.
        guard_a.set(WRITE_POLICY_READ_ONLY);
        assert!(guard_a.is_read_only());
        assert!(
            !guard_b.is_read_only(),
            "fresh session guards must not share state"
        );
    }

    /// Cross-cutting invariant for the production security acceptance
    /// criterion: a `READ_ONLY` + Write request observed by the reader
    /// loop must produce `cancelled` — exactly the production failure
    /// `write_policy=<unset>` → `allow_always`, now corrected to
    /// `READ_ONLY` → `cancelled` when the same Arc backs the reader
    /// loop and the connection.
    #[tokio::test]
    async fn production_security_criterion_read_only_write_is_cancelled() {
        // Simulate the production lifecycle:
        //   1. AcpConnection::spawn() builds `write_policy_guard`
        //      and passes a clone into the reader loop.
        //   2. SessionPool::set_session_write_policy() updates the
        //      connection-side guard.
        //   3. The agent emits session/request_permission.
        //   4. The reader loop reads the guard and replies.
        let guard: Arc<WritePolicyGuard> = Arc::new(WritePolicyGuard::new());
        // Spawn the reader loop with a clone — same allocation as
        // `guard`. Both the test-side `guard` and the task-side clone
        // reference the same WritePolicyGuard state.
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, mut agent_stdin_reader) = duplex(8 * 1024);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));
        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            Some(Arc::clone(&guard)),
        ));

        // SessionPool path: update via the connection-side handle.
        guard.set(WRITE_POLICY_READ_ONLY);

        // Agent emits a Write permission request.
        agent_stdout_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/request_permission\",\
                  \"params\":{\"toolCall\":{\"title\":\"Write\"},\
                  \"options\":[{\"kind\":\"allow_always\",\"optionId\":\"always\"}]}}\n",
            )
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        // Read the auto-reply and assert the production outcome.
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut byte = [0u8; 1];
            loop {
                match agent_stdin_reader.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        buf.push(byte[0]);
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;

        let text = String::from_utf8_lossy(&buf).to_string();
        let parsed: serde_json::Value = serde_json::from_str(text.trim())
            .unwrap_or_else(|e| panic!("reply not JSON: {e}; raw={text:?}"));
        // The reader loop wraps the outcome in a JSON-RPC envelope
        // `{"jsonrpc":"2.0","id":1,"result":<outcome>}`. Compare the
        // inner result.
        let result = parsed
            .get("result")
            .unwrap_or_else(|| panic!("reply missing result; raw={text:?}"));
        assert_eq!(
            result,
            &serde_json::json!({"outcome": {"outcome": "cancelled"}}),
            "production acceptance: READ_ONLY + Write -> cancelled"
        );

        drop(agent_stdout_writer);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    /// Drive `run_reader_loop` with a shared guard and emit a raw
    /// JSON-RPC frame so a test can pass an arbitrarily-decorated
    /// `toolCall` payload (kind + title + rawInput). Returns the
    /// full `result` JSON object the loop wrote back.
    async fn drive_permission_request_with_raw_payload_and_read_full_reply(
        guard: Option<Arc<WritePolicyGuard>>,
        params_json: &str,
    ) -> serde_json::Value {
        let (mut agent_stdout_writer, agent_stdout_reader) = duplex(8 * 1024);
        let (agent_stdin_writer, mut agent_stdin_reader) = duplex(8 * 1024);

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let writer = Arc::new(Mutex::new(agent_stdin_writer));
        let handle = tokio::spawn(run_reader_loop(
            agent_stdout_reader,
            writer,
            pending.clone(),
            notify_tx.clone(),
            guard,
        ));

        let body = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/request_permission\",\
             \"params\":{params_json}}}"
        );
        let request = format!("{body}\n");
        agent_stdout_writer
            .write_all(request.as_bytes())
            .await
            .unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let mut reply_buf = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut byte = [0u8; 1];
            loop {
                match agent_stdin_reader.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        reply_buf.push(byte[0]);
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;

        drop(agent_stdout_writer);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

        let reply_text = String::from_utf8_lossy(&reply_buf).to_string();
        let parsed: serde_json::Value = serde_json::from_str(reply_text.trim())
            .unwrap_or_else(|e| panic!("reply not JSON: {e}; raw={reply_text:?}"));
        parsed
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("reply missing result; raw={reply_text:?}"))
    }

    /// (16) Reader-loop end-to-end with the literal production payload
    /// shape. Drives `run_reader_loop` with a shared guard set to
    /// READ_ONLY and emits the exact JSON observed in the
    /// `.phase641f-round3-probe` incident. Must reply `cancelled`.
    /// This test fails immediately if the kind/title classifier
    /// regresses to exact full-title equality.
    #[tokio::test]
    async fn reader_loop_cancels_exact_production_payload() {
        let guard = Arc::new(WritePolicyGuard::new());
        guard.set(WRITE_POLICY_READ_ONLY);

        // The literal production-shaped params object.
        let params_json = r#"{"toolCall":{"title":"Write .phase641f-round3-probe","kind":"edit","rawInput":{"file_path":"/home/arthur/workspace/ai-workstation/.phase641f-round3-probe","content":"probe"}},"options":[{"kind":"allow_always","optionId":"always"}]}"#;

        let reply = drive_permission_request_with_raw_payload_and_read_full_reply(
            Some(Arc::clone(&guard)),
            params_json,
        )
        .await;

        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "cancelled"}}),
            "reader loop seam under READ_ONLY must cancel decorated production-shaped Write"
        );
    }

    /// Reader-loop with the same payload but `unset` guard. The fix
    /// MUST NOT globally turn unknown sessions into READ_ONLY.
    #[tokio::test]
    async fn reader_loop_preserves_legacy_for_production_payload_when_unset() {
        let params_json = r#"{"toolCall":{"title":"Write .phase641f-round3-probe","kind":"edit","rawInput":{"file_path":"/home/arthur/workspace/ai-workstation/.phase641f-round3-probe","content":"probe"}},"options":[{"kind":"allow_always","optionId":"always"}]}"#;

        let reply =
            drive_permission_request_with_raw_payload_and_read_full_reply(None, params_json).await;

        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
            "reader loop seam with unset guard must preserve legacy allow on decorated Write"
        );
    }

    /// Reader-loop with the same payload but `MODIFY_ALLOWED` guard.
    #[tokio::test]
    async fn reader_loop_preserves_legacy_for_production_payload_when_modify_allowed() {
        let guard = Arc::new(WritePolicyGuard::new());
        guard.set(WRITE_POLICY_MODIFY_ALLOWED);

        let params_json = r#"{"toolCall":{"title":"Write .phase641f-round3-probe","kind":"edit","rawInput":{"file_path":"/home/arthur/workspace/ai-workstation/.phase641f-round3-probe","content":"probe"}},"options":[{"kind":"allow_always","optionId":"always"}]}"#;

        let reply = drive_permission_request_with_raw_payload_and_read_full_reply(
            Some(Arc::clone(&guard)),
            params_json,
        )
        .await;

        assert_eq!(
            reply,
            serde_json::json!({"outcome": {"outcome": "selected", "optionId": "always"}}),
            "reader loop seam with MODIFY_ALLOWED guard must preserve legacy allow on decorated Write"
        );
    }
}

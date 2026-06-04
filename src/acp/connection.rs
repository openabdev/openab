use crate::acp::protocol::{
    parse_config_options, ConfigOption, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
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
    pub config_options: Vec<ConfigOption>,
    pub last_active: Instant,
    pub session_reset: bool,
    /// Ring buffer of recent agent stderr lines. Surfaces to the user when the
    /// JSON-RPC error envelope is opaque (e.g. `-32603` with `data: {}` from
    /// opencode, or agents that omit `data` entirely like hermes-agent).
    /// Capped at `STDERR_TAIL_CAPACITY` lines; oldest evicted on overflow.
    /// Not written to disk — purely in-memory for the lifetime of the session.
    /// #998, #1000: complement to PR #885's `data.message` extraction.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    _reader_handle: JoinHandle<()>,
    _stderr_handle: Option<JoinHandle<()>>,
}

/// Maximum number of recent stderr lines kept in the per-session ring buffer.
/// Sized so a full snapshot stays under 25 KB even with max-line-cap lines.
pub const STDERR_TAIL_CAPACITY: usize = 50;

/// Maximum length of a single stderr line stored in the ring buffer.
/// Agents occasionally emit full stack traces or JSON dumps on a single
/// line; without a cap, one such line blows the per-snapshot memory budget
/// and the `clone()` on every error path. 500 chars is enough to capture
/// typical error messages (path + reason + 1-2 lines of context) while
/// rejecting megabyte-class pathological output.
pub const STDERR_LINE_MAX: usize = 500;

/// Suffix appended to truncated stderr lines so the user knows context was
/// cut. Keep lowercase ASCII; matches the sanitization style of the rest
/// of the line.
const STDERR_TRUNCATED_SUFFIX: &str = " [truncated]";

/// Minimum number of characters that must follow a secret prefix before we
/// mask it. Chosen so common false-positive substrings ("skill", "skip",
/// "sketch") are not masked while real keys (always 30+ chars total) are.
const SECRET_MIN_KEY_LENGTH: usize = 12;

/// Mask common credential patterns in user-facing stderr output. Conservative
/// (over-redacts rather than under-redacts): an unmatched pattern is safer
/// than a leaked token, since the worst false positive is a slightly uglier
/// error message.
///
/// Patterns covered (with vendor reference):
/// - `sk-ant-...`           Anthropic API key
/// - `sk-...`               OpenAI API key (length-gated to skip "skill" etc.)
/// - `ghp_...`              GitHub classic PAT
/// - `github_pat_...`       GitHub fine-grained PAT
/// - `xoxb-...` / `xoxp-...` Slack bot/user token
/// - `Bearer <token>`       Authorization header
/// - `-----BEGIN ... PRIVATE KEY-----` PEM private key
/// - `*_API_KEY=...` / `*_TOKEN=...` / `*_SECRET=...` env-style assignment
///
/// This is NOT exhaustive. Maintainer audit may extend with AWS, Stripe,
/// Discord bot tokens, etc. Documented in PR #1003 as a known limitation.
pub(crate) fn redact_stderr_line(line: &str) -> String {
    // Mask length-gated prefixes. Each pattern requires at least
    // SECRET_MIN_KEY_LENGTH characters after the prefix to be considered a
    // real key. Trailing alnum/+/= (base64url) is preserved; the rest of the
    // surrounding text is left intact.
    let gated_prefixes: &[(&str, &str)] = &[
        ("sk-ant-", "[REDACTED:anthropic-key]"),
        ("sk-", "[REDACTED:openai-key]"),
        ("ghp_", "[REDACTED:github-pat]"),
        ("github_pat_", "[REDACTED:github-fine-grained-pat]"),
        ("xoxb-", "[REDACTED:slack-bot-token]"),
        ("xoxp-", "[REDACTED:slack-user-token]"),
    ];

    let mut out = line.to_string();
    for &(prefix, replacement) in gated_prefixes {
        // Replace every occurrence of `prefix` in the line. A line may carry
        // multiple keys (e.g. an env-dump at process startup), and we must
        // mask all of them. Each match is independently length-gated, so
        // short substrings like "sk-abc" are not over-masked.
        let mut search_from = 0;
        while let Some(rel_start) = out[search_from..].find(prefix) {
            let start = search_from + rel_start;
            let after = start + prefix.len();
            let tail = &out[after..];
            // Body run: alnum / `_` / `-` / `+` / `=` (base64url alphabet).
            // Stop at the first non-body char.
            let body_byte_len: usize = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '='))
                .map(|c| c.len_utf8())
                .sum();
            if body_byte_len == 0 || body_byte_len < SECRET_MIN_KEY_LENGTH {
                search_from = after;
                continue;
            }
            let body_end = after + body_byte_len;
            out.replace_range(start..body_end, replacement);
            // Advance past the replacement (which contains the literal
            // "[REDACTED:...]" — no risk of re-matching the prefix).
            search_from = start + replacement.len();
        }
    }

    // Authorization: Bearer <token>
    if let Some(idx) = out.find("Bearer ") {
        // Skip past "Bearer " (7 chars) and find end of token.
        let after = idx + "Bearer ".len();
        let tail = &out[after..];
        let body_len = tail
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ',')
            .count();
        if body_len >= SECRET_MIN_KEY_LENGTH {
            let body_end = after
                + tail
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != ',')
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
            out.replace_range(after..body_end, "[REDACTED:bearer-token]");
        }
    }

    // PEM private key headers (line-by-line, no length gate — always redact).
    if out.contains("-----BEGIN") && out.contains("PRIVATE KEY-----") {
        if let Some(start) = out.find("-----BEGIN") {
            if let Some(end) = out[start..].find("PRIVATE KEY-----") {
                let abs_end = start + end + "PRIVATE KEY-----".len();
                out.replace_range(start..abs_end, "-----BEGIN [REDACTED:private-key]-----");
            }
        }
    }

    // Env-style assignments: *_API_KEY=val, *_TOKEN=val, *_SECRET=val,
    // *_KEY=val. Match common suffixes; the value runs to end-of-string or
    // next whitespace, then is masked.
    let env_suffixes = [
        "_API_KEY=",
        "_TOKEN=",
        "_SECRET=",
        "_KEY=",
    ];
    for suffix in env_suffixes {
        // Find every occurrence and redact the value. (Multiple matches per
        // line are possible in startup dumps.)
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(suffix) {
            let start = search_from + rel + suffix.len();
            // Skip optional leading quote.
            let value_start = if out[start..].starts_with('"') || out[start..].starts_with('\'') {
                start + 1
            } else {
                start
            };
            let tail = &out[value_start..];
            let body_len = tail
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != ',' && *c != ';' && *c != '"' && *c != '\'')
                .count();
            if body_len > 0 {
                let body_end = value_start
                    + tail
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ',' && *c != ';' && *c != '"' && *c != '\'')
                        .map(|c| c.len_utf8())
                        .sum::<usize>();
                out.replace_range(value_start..body_end, "[REDACTED]");
                search_from = body_end;
            } else {
                search_from = value_start;
            }
        }
    }

    out
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

                let outcome = build_permission_response(msg.params.as_ref());
                info!(title, %outcome, "auto-respond permission");
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
        // for logging; clients MAY capture or ignore this).
        //
        // Each sanitized line is also pushed to a per-session ring buffer so
        // that opaque JSON-RPC errors (e.g. -32603 with `data: {}` from
        // opencode) can surface the real cause to the user. See #1000 / #998.
        //
        // Before reaching the user-facing ring buffer, each line is:
        //   1. Sanitized (control chars stripped except tab)
        //   2. Length-capped at STDERR_LINE_MAX with a "[truncated]" suffix
        //   3. Secret-redacted via redact_stderr_line (PR #1003 review ask)
        // Steps 1+2 are applied to both the operator log and the ring buffer;
        // step 3 (redaction) is applied to both paths for symmetry — the
        // operator log would otherwise leak a token to kubectl logs even
        // though the user-facing Discord message is safe.
        let stderr_buffer: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_CAPACITY)));
        let stderr_handle = if let Some(stderr) = proc.stderr.take() {
            let cmd_name = command.to_string();
            let stderr_buffer = Arc::clone(&stderr_buffer);
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
                                let sanitized: String = trimmed.chars()
                                    .filter(|c| !c.is_control() || *c == '\t')
                                    .collect();
                                if !sanitized.is_empty() {
                                    // Length-cap before redacting — the cap
                                    // is a bound on the *post*-redaction size
                                    // (replacement strings are fixed length).
                                    let capped = if sanitized.len() > STDERR_LINE_MAX {
                                        let mut s = sanitized;
                                        s.truncate(STDERR_LINE_MAX);
                                        s.push_str(STDERR_TRUNCATED_SUFFIX);
                                        s
                                    } else {
                                        sanitized
                                    };
                                    let redacted = redact_stderr_line(&capped);
                                    tracing::warn!(agent = %cmd_name, "{redacted}");
                                    // Push to ring buffer; evict oldest on overflow.
                                    let mut buf = stderr_buffer.lock().await;
                                    if buf.len() >= STDERR_TAIL_CAPACITY {
                                        buf.pop_front();
                                    }
                                    buf.push_back(redacted);
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

        let reader_handle = tokio::spawn(run_reader_loop(
            stdout,
            stdin.clone(),
            pending.clone(),
            notify_tx.clone(),
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
            config_options: Vec::new(),
            last_active: Instant::now(),
            session_reset: false,
            stderr_tail: stderr_buffer,
            _reader_handle: reader_handle,
            _stderr_handle: stderr_handle,
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Snapshot the most recent stderr lines for inclusion in coded error
    /// display. Clones the entire ring buffer; caller can pass the result
    /// to `format_coded_error` so opaque -32603 errors (data: {} or no
    /// data) show the agent's actual failure reason.
    ///
    /// Returns lines in chronological order (oldest first, newest last).
    /// Returns an empty Vec if the agent has not produced any stderr yet
    /// or stderr was never captured (e.g. process group re-exec edge case).
    pub async fn stderr_tail_snapshot(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .await
            .iter()
            .cloned()
            .collect()
    }

    pub(crate) async fn send_raw(&self, data: &str) -> Result<()> {
        debug!(data = data.trim(), "acp_send");
        let mut w = self.stdin.lock().await;
        w.write_all(data.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
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

    /// Send a prompt with content blocks (text and/or images) and return a receiver
    /// for streaming notifications. The final message on the channel will have id set
    /// (the prompt response).
    pub async fn session_prompt(
        &mut self,
        content_blocks: Vec<ContentBlock>,
    ) -> Result<(mpsc::UnboundedReceiver<JsonRpcMessage>, u64)> {
        self.last_active = Instant::now();

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
        build_agent_env, build_permission_response, pick_best_option, redact_stderr_line,
        STDERR_LINE_MAX, STDERR_TRUNCATED_SUFFIX,
    };
    use serde_json::json;

    // ─── redact_stderr_line tests (PR #1003 review ask) ────────────────────
    //
    // Test fixtures use obviously-fake body strings (`TESTKEYFAKEBODY_...`)
    // rather than realistic-looking base64, so that GitHub's secret-scanner
    // does not flag the unit test source as a leaked key. The fixtures still
    // satisfy `SECRET_MIN_KEY_LENGTH` (>= 12 chars) so they exercise the
    // length-gate redaction path that real keys hit.

    /// Each gated prefix redacts when the key is long enough to be real.
    /// Pattern: `"prefix + 12+ alnum/+/= chars" → [REDACTED:...]`
    #[test]
    fn redact_anthropic_key() {
        let key = "sk-ant-api03-TESTKEYFAKEBODY_NOT_A_REAL_KEY_aaaaaaaa";
        let line = format!("Error: invalid key {key}");
        let out = redact_stderr_line(&line);
        assert!(out.contains("[REDACTED:anthropic-key]"), "got: {out}");
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    #[test]
    fn redact_openai_key() {
        let key = "sk-TESTKEYFAKEBODY_OPENAI_NOT_REAL_bbbbbbbbbbbb";
        let line = format!("Bearer {key}");
        // Note: "Bearer " match runs first and may catch this; ensure either
        // bearer or openai path masked the secret. Either is acceptable.
        let out = redact_stderr_line(&line);
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    #[test]
    fn redact_github_pat() {
        let key = "ghp_TESTKEYFAKEBODY_GHPAT_NOT_REAL_cccccccccccc";
        let line = format!("auth failed for {key}");
        let out = redact_stderr_line(&line);
        assert!(out.contains("[REDACTED:github-pat]"), "got: {out}");
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    #[test]
    fn redact_github_fine_grained_pat() {
        let key = "github_pat_TESTKEYFAKEBODY_FGPAT_NOT_REAL_dddddddddddddd";
        let line = format!("token: {key}");
        let out = redact_stderr_line(&line);
        assert!(out.contains("[REDACTED:github-fine-grained-pat]"), "got: {out}");
    }

    #[test]
    fn redact_slack_token() {
        // The literal Slack token prefix (`xoxb-`) is constructed at runtime
        // to keep GitHub's push-protection secret scanner from flagging this
        // test fixture as a leaked token. The redaction logic under test is
        // identical — only the way the input string is built differs.
        let slack_prefix = "xox".to_string() + "b-";
        let line = format!("{slack_prefix}1234567890-TESTKEYFAKEBODY_SLACK_NOT_REAL_eeeeee");
        let out = redact_stderr_line(&line);
        assert!(out.contains("[REDACTED:slack-bot-token]"), "got: {out}");
    }

    /// Length gate: short strings that start with a secret prefix but aren't
    /// real keys MUST NOT be masked (false-positive prevention).
    #[test]
    fn redact_does_not_mask_short_prefixes() {
        // "skill" starts with "sk" but not "sk-" — not a match.
        assert_eq!(redact_stderr_line("use your skill wisely"), "use your skill wisely");
        // "sk-abc" is too short to be a real key.
        assert_eq!(redact_stderr_line("sk-abc"), "sk-abc");
        // "sketch" doesn't match "sk-".
        assert_eq!(redact_stderr_line("sketch the design"), "sketch the design");
    }

    /// Authorization: Bearer <token> with a real-looking token.
    #[test]
    fn redact_bearer_token() {
        let line = "Authorization: Bearer TESTKEYFAKEBODY_BEARER_NOT_REAL_ffffffffff";
        let out = redact_stderr_line(line);
        assert!(out.contains("[REDACTED:bearer-token]"), "got: {out}");
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    /// PEM private key header redacts the BEGIN...PRIVATE KEY range.
    #[test]
    fn redact_pem_private_key() {
        let line = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let out = redact_stderr_line(line);
        assert!(out.contains("[REDACTED:private-key]"), "got: {out}");
        assert!(!out.contains("OPENSSH"), "leaked: {out}");
    }

    /// Env-style assignments redact the value side.
    #[test]
    fn redact_env_api_key_assignment() {
        let line = "Failed: ANTHROPIC_API_KEY=sk-ant-TESTKEYFAKEBODY_NOT_REAL_ggggg not set";
        let out = redact_stderr_line(&line);
        assert!(out.contains("[REDACTED]"), "got: {out}");
        // The sk-ant pattern may have already redacted the value before the
        // _API_KEY= match fires; either is acceptable as long as the literal
        // raw key body is not in the output.
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    /// Line that contains no secret patterns passes through unchanged.
    #[test]
    fn redact_passes_through_clean_lines() {
        let line = "Error: connection refused at port 8080";
        assert_eq!(redact_stderr_line(line), line);
    }

    /// Multiple secrets on one line each get masked.
    #[test]
    fn redact_multiple_secrets_on_one_line() {
        let line = "headers: Authorization: Bearer TESTKEYFAKEBODY_BEARER_NOT_REAL_aaaaaa, key=ghp_TESTKEYFAKEBODY_GHPAT_NOT_REAL_bbbbbb";
        let out = redact_stderr_line(line);
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
    }

    /// Same prefix appearing twice on one line both get masked. Regression
    /// for a prior version that did `out.find()` once per prefix and missed
    /// subsequent occurrences in the same line.
    #[test]
    fn redact_same_prefix_twice_on_one_line() {
        let line = "dumping env: FOO=sk-ant-TESTKEYFAKEBODY_NOT_REAL_aaaaaa BAR=sk-ant-TESTKEYFAKEBODY_NOT_REAL_bbbbbb";
        let out = redact_stderr_line(&line);
        assert!(!out.contains("TESTKEYFAKEBODY"), "leaked: {out}");
        // Each occurrence must be replaced (replacement string is the same,
        // so we check that the original body run does not appear twice).
        let count_before_a = line.matches("TESTKEYFAKEBODY_NOT_REAL_aaaaaa").count();
        let count_after_a = out.matches("TESTKEYFAKEBODY_NOT_REAL_aaaaaa").count();
        assert_eq!(count_before_a, 1);
        assert_eq!(count_after_a, 0);
    }

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
        ));

        let stale = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{\"stopReason\":\"ok\"}}\n";
        agent_stdout_writer.write_all(stale).await.unwrap();
        agent_stdout_writer.flush().await.unwrap();

        let forwarded = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sub_rx.recv(),
        )
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
}

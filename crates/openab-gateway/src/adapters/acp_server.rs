//! ACP (Agent Client Protocol) Server adapter.
//!
//! Exposes OAB as an ACP-compliant server over WebSocket at `GET /acp`.
//! Any ACP client (Zed, JetBrains, desktop apps, web apps, CLIs) can connect
//! and interact with OAB's multi-agent platform using the standard protocol.
//!
//! Protocol flow:
//!   Client connects via WebSocket → sends `initialize` → `session/new` → `session/prompt`
//!   Server streams back `AgentMessageChunk` notifications, then the prompt response.
//!
//! Internally, prompts are converted to `GatewayEvent` and dispatched through OAB's
//! existing event pipeline. Replies (`GatewayReply`) are translated back into ACP
//! notifications and streamed to the client.

use crate::schema::*;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// ACP wire protocol MAJOR version (an integer), returned from `initialize`.
/// Tracks the official schema — see `docs/acp-official-methods.md`.
const ACP_PROTOCOL_VERSION: u32 = 1;

/// Lightweight per-connection resource caps: turn unbounded client-driven growth into
/// a deterministic overload error. Full backpressure (bounded outbound channel), idle
/// eviction, and global connection/worker limits are a follow-up (review F6, roadmap).
const MAX_SESSIONS_PER_CONNECTION: usize = 128;
const MAX_INFLIGHT_PROMPTS: usize = 32;
const MAX_FRAME_BYTES: usize = 1 << 20; // 1 MiB per inbound JSON-RPC frame
/// JSON-RPC implementation-defined server error for a hit resource cap.
const ACP_OVERLOADED: i32 = -32000;

// ---------------------------------------------------------------------------
// ACP Configuration
// ---------------------------------------------------------------------------

pub struct AcpConfig {
    pub auth_key: Option<String>,
}

impl AcpConfig {
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("OPENAB_ACP_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        let auth_key = std::env::var("OPENAB_ACP_AUTH_KEY").ok();
        if auth_key.is_none() {
            warn!("OPENAB_ACP_AUTH_KEY not set — ACP endpoint is UNAUTHENTICATED");
        }
        Some(Self { auth_key })
    }
}

/// Incremental text to stream, given the bytes already sent (`sent_len`) and the
/// latest full-text snapshot. Slices via `str::get` (never byte-index `[..]`), so a
/// `sent_len` that lands mid-codepoint — possible with CJK / 顏文字 / emoji only on a
/// non-append snapshot rewrite — yields `None` (caller skips the frame; the next
/// snapshot re-covers) instead of panicking. In the normal append case `sent_len` is
/// always the byte length of a prior valid snapshot and therefore a char boundary of
/// the new text, so a multi-byte codepoint is always emitted whole, never split.
/// Returns `None` when there is nothing new to send.
fn stream_delta(sent_len: usize, full_text: &str) -> Option<&str> {
    match full_text.get(sent_len..) {
        Some(d) if !d.is_empty() => Some(d),
        _ => None,
    }
}

/// Whether ACP frame tracing is on (`OPENAB_ACP_TRACE=1|true`). When set, every
/// JSON-RPC frame on the upstream client↔gateway hop is logged (at `debug!`) in both
/// directions (`dir="in"` / `dir="out"`).
///
/// **This is an opt-in debugging tool that records message CONTENT** — prompts, replies,
/// and negotiated capabilities appear in the logs (truncated, see `trace_frame`). It is
/// off by default and emits at `debug!` so it never surfaces at the default log level;
/// only enable it in a trusted environment when you need to inspect real ACP traffic
/// (e.g. to validate the generated-type round-trip against what clients/agents emit).
pub(crate) fn acp_trace_enabled() -> bool {
    std::env::var("OPENAB_ACP_TRACE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// Truncate a frame for trace logging so a large prompt/reply doesn't dump a huge line
/// (and doesn't record the *complete* content). Keeps the first `CAP` scalar values.
fn trace_frame(s: &str) -> std::borrow::Cow<'_, str> {
    const CAP: usize = 512;
    let total = s.chars().count();
    if total <= CAP {
        return std::borrow::Cow::Borrowed(s);
    }
    let end = s.char_indices().nth(CAP).map_or(s.len(), |(i, _)| i);
    std::borrow::Cow::Owned(format!("{}…(+{} chars)", &s[..end], total - CAP))
}

/// Validate a request's `params` against a generated ACP request type `T`, returning a
/// JSON-RPC `-32602` message when a required field is missing or malformed. This checks
/// shape only — the base validates `cwd`/`mcpServers` for conformance but does not yet
/// propagate them (see the base ADR §5); missing `params` is itself invalid.
fn validate_params<T: serde::de::DeserializeOwned>(params: Option<&Value>) -> Result<(), String> {
    let value = params.cloned().unwrap_or(Value::Null);
    serde_json::from_value::<T>(value)
        .map(|_| ())
        .map_err(|e| format!("Invalid params: {e}"))
}

// ---------------------------------------------------------------------------
// ACP Session tracking
// ---------------------------------------------------------------------------

/// Tracks an active ACP session.
struct AcpSession {
    /// Channel ID used in GatewayEvent (maps replies back to this session)
    channel_id: String,
    /// Whether a prompt is currently in-flight for this session
    busy: bool,
    /// Cancel signal for the in-flight prompt, if any. `session/cancel` fires
    /// this so the streaming task stops gracefully and returns `stopReason:
    /// "cancelled"` to the prompt's own request id (rather than hard-aborting
    /// the task and orphaning that id).
    cancel: Option<Arc<tokio::sync::Notify>>,
}

pub enum ReplyChunk {
    /// Incremental text snapshot (full text so far)
    Text(String),
    /// Agent finished responding
    Done,
}

/// Registry of active ACP sessions: channel_id → reply sender.
/// Uses std::sync::Mutex because all operations are fast CPU-bound
/// (insert/remove/get) and never hold the lock across .await.
pub type AcpReplyRegistry = Arc<std::sync::Mutex<HashMap<String, mpsc::UnboundedSender<ReplyChunk>>>>;

pub fn new_reply_registry() -> AcpReplyRegistry {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// JSON-RPC types (minimal subset for ACP)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: Value,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket upgrade handler: GET /acp
// ---------------------------------------------------------------------------

pub async fn ws_upgrade(
    State(state): State<Arc<crate::AppState>>,
    query: Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    // Auth: Bearer token from Authorization header or ?token= query param
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| query.get("token").map(|s| s.as_str()));

    let expected = state.acp.as_ref().and_then(|c| c.auth_key.as_ref());
    if let Some(expected) = expected {
        let valid = match token {
            Some(t) => {
                // Constant-time comparison to prevent timing attacks
                use subtle::ConstantTimeEq;
                t.as_bytes().ct_eq(expected.as_bytes()).into()
            }
            None => false,
        };
        if !valid {
            warn!("ACP WebSocket rejected: invalid or missing token");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_acp_connection(state, socket))
}

// ---------------------------------------------------------------------------
// ACP Connection handler
// ---------------------------------------------------------------------------

async fn handle_acp_connection(state: Arc<crate::AppState>, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let connection_id = format!("acp_conn_{}", Uuid::new_v4());

    info!(connection = %connection_id, "ACP client connected");

    // Frame tracing (OPENAB_ACP_TRACE) — read once per connection.
    let trace = acp_trace_enabled();

    // Session state for this connection
    let sessions: Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let mut initialized = false;

    // Track spawned prompt tasks so we can abort on disconnect
    let mut prompt_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Channel for sending messages back to the client
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    // Forward outbound messages to WebSocket. Single choke point for every outbound
    // frame, so trace here rather than at each send site.
    let send_conn = connection_id.clone();
    let send_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if trace {
                debug!(connection = %send_conn, dir = "out", frame = %trace_frame(&msg), "ACP frame");
            }
            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Process incoming messages
    while let Some(Ok(msg)) = ws_rx.next().await {
        let Message::Text(text) = msg else {
            continue;
        };

        // Bound inbound frame size before parsing (deterministic overload, not OOM).
        if text.len() > MAX_FRAME_BYTES {
            let resp = JsonRpcResponse::error(
                Value::Null,
                ACP_OVERLOADED,
                format!("Frame too large ({} bytes; max {MAX_FRAME_BYTES})", text.len()),
            );
            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            continue;
        }

        if trace {
            debug!(connection = %connection_id, dir = "in", frame = %trace_frame(&text), "ACP frame");
        }

        let raw: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                let err_resp =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"));
                let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
                continue;
            }
        };

        // JSON-RPC: a message WITHOUT an `id` member is a notification and MUST NOT
        // receive any response; a message WITH an `id` (including explicit `null`) is a
        // request. serde's `Option<Value>` collapses omitted and `null` to the same
        // `None`, so notification detection uses raw key PRESENCE on the parsed JSON.
        let is_notification = raw.get("id").is_none();

        let req: JsonRpcRequest = match serde_json::from_value(raw) {
            Ok(r) => r,
            Err(e) => {
                if !is_notification {
                    let err_resp =
                        JsonRpcResponse::error(Value::Null, -32600, format!("Invalid Request: {e}"));
                    let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
                }
                continue;
            }
        };

        // Validate JSON-RPC version (spec requires "2.0"). Only answer a request.
        if req.jsonrpc != "2.0" {
            if !is_notification {
                let id = req.id.clone().unwrap_or(Value::Null);
                let err_resp =
                    JsonRpcResponse::error(id, -32600, "Invalid Request: jsonrpc must be \"2.0\"");
                let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
            }
            continue;
        }

        // Request-only methods sent as a notification (no id) cannot return their result,
        // so per JSON-RPC they get no response — and we do not execute them as
        // fire-and-forget. Only `session/cancel` is a real notification (handled below).
        if is_notification
            && matches!(
                req.method.as_str(),
                "initialize" | "session/new" | "session/resume" | "session/prompt"
            )
        {
            debug!(method = %req.method, "ACP request-only method sent without id (notification) — ignored");
            continue;
        }

        // Safe: request-only arms below are only reached when `id` is present.
        let id = req.id.clone().unwrap_or(Value::Null);

        match req.method.as_str() {
            "initialize" => {
                let resp = handle_initialize(&req);
                // Only mark the connection initialized when negotiation succeeded.
                let negotiated_ok = resp.error.is_none();
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                if negotiated_ok {
                    initialized = true;
                }
            }
            "session/new" => {
                if !initialized {
                    let resp = JsonRpcResponse::error(id, -32002, "Not initialized");
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                // Required params per schema: { cwd, mcpServers }.
                if let Err(msg) =
                    validate_params::<crate::adapters::acp_schema::NewSessionRequest>(req.params.as_ref())
                {
                    let resp = JsonRpcResponse::error(id, -32602, msg);
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                // Cap sessions per connection (deterministic overload, not unbounded).
                if sessions.lock().await.len() >= MAX_SESSIONS_PER_CONNECTION {
                    let resp = JsonRpcResponse::error(
                        id,
                        ACP_OVERLOADED,
                        format!("Too many sessions on this connection (max {MAX_SESSIONS_PER_CONNECTION})"),
                    );
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                let resp = handle_session_new(&sessions, id.clone()).await;
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            }
            "session/resume" => {
                if !initialized {
                    let resp = JsonRpcResponse::error(id, -32002, "Not initialized");
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                // Required params per schema: { sessionId, cwd, mcpServers? }. The
                // sessionId's `sess_<uuid>` shape is checked further in the handler.
                if let Err(msg) =
                    validate_params::<crate::adapters::acp_schema::ResumeSessionRequest>(req.params.as_ref())
                {
                    let resp = JsonRpcResponse::error(id, -32602, msg);
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                let resp = handle_session_resume(&sessions, id.clone(), req.params.as_ref()).await;
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            }
            "session/prompt" => {
                if !initialized {
                    let resp = JsonRpcResponse::error(id, -32002, "Not initialized");
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                // Cap concurrent in-flight prompts per connection (drop finished first).
                prompt_tasks.retain(|h| !h.is_finished());
                if prompt_tasks.len() >= MAX_INFLIGHT_PROMPTS {
                    let resp = JsonRpcResponse::error(
                        id,
                        ACP_OVERLOADED,
                        format!("Too many in-flight prompts (max {MAX_INFLIGHT_PROMPTS})"),
                    );
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    continue;
                }
                // session/prompt is async — spawn a task to handle streaming
                let state_clone = state.clone();
                let sessions_clone = sessions.clone();
                let out_tx_clone = out_tx.clone();
                let handle = tokio::spawn(async move {
                    handle_session_prompt(
                        &state_clone,
                        &sessions_clone,
                        id,
                        req.params.as_ref(),
                        &out_tx_clone,
                    )
                    .await;
                });
                prompt_tasks.push(handle);
            }
            "session/cancel" => {
                // Per ACP, session/cancel is a one-way NOTIFICATION — no response.
                // Fire the session's cancel signal; the in-flight prompt observes
                // it, cleans up, and returns stopReason:"cancelled" to the prompt's
                // own request id.
                let sess_key = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("sessionId"))
                    .and_then(|v| v.as_str());
                if let Some(k) = sess_key {
                    let notify = sessions.lock().await.get(k).and_then(|s| s.cancel.clone());
                    if let Some(n) = notify {
                        n.notify_one();
                    }
                }
                // `session/cancel` is a notification: no response when sent as one. If a
                // client sent it as a request (with an id), acknowledge with an empty result.
                if !is_notification {
                    let resp = JsonRpcResponse::success(id, json!({}));
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                }
            }
            _ => {
                // Unknown method: error a request; ignore an unknown notification.
                if !is_notification {
                    let resp = JsonRpcResponse::error(
                        id,
                        -32601,
                        format!("Method not found: {}", req.method),
                    );
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                }
            }
        }

        // Clean up finished tasks
        prompt_tasks.retain(|h| !h.is_finished());
    }

    // --- Disconnect cleanup ---
    // Abort any in-flight prompt tasks to prevent registry leaks
    for handle in prompt_tasks {
        handle.abort();
    }

    // Remove all sessions for this connection from the reply registry
    if let Some(ref registry) = state.acp_reply_registry {
        let sessions_guard = sessions.lock().await;
        let channel_ids: Vec<String> = sessions_guard
            .values()
            .map(|s| s.channel_id.clone())
            .collect();
        drop(sessions_guard);

        let mut reg = registry.lock().unwrap_or_else(|e| e.into_inner());
        for cid in &channel_ids {
            reg.remove(cid);
        }
        debug!(
            connection = %connection_id,
            sessions_cleaned = channel_ids.len(),
            "ACP connection cleanup complete"
        );
    }

    send_task.abort();
    info!(connection = %connection_id, "ACP client disconnected");
}

// ---------------------------------------------------------------------------
// Method handlers
// ---------------------------------------------------------------------------

fn handle_initialize(req: &JsonRpcRequest) -> JsonRpcResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    // Validate the official request (protocolVersion is required) before negotiating.
    let init: crate::adapters::acp_schema::InitializeRequest =
        match serde_json::from_value(req.params.clone().unwrap_or(Value::Null)) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::error(id, -32602, format!("Invalid initialize params: {e}"));
            }
        };
    // Negotiate: respond with the version we will use = the lower of the client's and
    // ours. A higher client version negotiates down to ours (the client then decides);
    // a version below our minimum (v1 is the first ACP version) cannot be satisfied.
    let client_version = *init.protocol_version;
    let negotiated = client_version.min(ACP_PROTOCOL_VERSION as u16);
    if negotiated < 1 {
        return JsonRpcResponse::error(
            id,
            -32602,
            format!("Unsupported protocolVersion {client_version}; this agent supports {ACP_PROTOCOL_VERSION}"),
        );
    }
    // ACP initialize response. We advertise `sessionCapabilities.resume` (we support
    // session/resume) but NOT `loadSession` — the gateway cannot replay conversation
    // history to the client (it lives inside the downstream agent CLI).
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": negotiated,
            "agentCapabilities": {
                "loadSession": false,
                "sessionCapabilities": {
                    "resume": {}
                },
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false
                }
            },
            "agentInfo": {
                "name": "openab",
                "title": "OpenAB",
                "version": env!("CARGO_PKG_VERSION")
            },
            "authMethods": []
        }),
    )
}

async fn handle_session_new(
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    id: Value,
) -> JsonRpcResponse {
    // sessionId and channel_id share one uuid so channel_id is always
    // re-derivable from a persisted sessionId (see session/resume).
    let uuid = Uuid::new_v4();
    let session_id = format!("sess_{uuid}");
    let channel_id = format!("acp_{uuid}");

    sessions.lock().await.insert(
        session_id.clone(),
        AcpSession {
            channel_id,
            busy: false,
            cancel: None,
        },
    );

    info!(session = %session_id, "ACP session created");

    // ACP session/new response is just { sessionId }.
    JsonRpcResponse::success(id, json!({ "sessionId": session_id }))
}

/// `session/resume` — re-attach to a session the client persisted, WITHOUT
/// replaying history (per ACP: the agent MUST NOT replay via session/update).
///
/// The client re-presents its `sessionId`; we derive the same deterministic
/// `channel_id`, so the next prompt's GatewayEvent maps to the same core
/// `session_key` (`acp:<channel_id>`) and the existing conversation continues.
/// The core recovers the underlying agent session via its own persisted mapping
/// plus a downstream `session/load` (survives process restart within the agent's
/// retention / `session_ttl_hours`). Whether that succeeds is not observable
/// here — an expired session simply starts fresh, and the core prefixes its
/// first reply with a "Session expired" notice the client can surface.
///
/// Security: `sessionId` is a server-minted, high-entropy capability;
/// `derive_channel_id` requires a well-formed `sess_<uuid>`, keeping the channel
/// inside the `acp_` namespace and rejecting forged ids.
async fn handle_session_resume(
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    id: Value,
    params: Option<&Value>,
) -> JsonRpcResponse {
    let session_id = match params.and_then(|p| p.get("sessionId")).and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return JsonRpcResponse::error(id, -32602, "Missing sessionId"),
    };

    let channel_id = match derive_channel_id(&session_id) {
        Some(cid) => cid,
        None => {
            return JsonRpcResponse::error(
                id,
                -32602,
                "Invalid sessionId: expected the form sess_<uuid>",
            );
        }
    };

    sessions.lock().await.insert(
        session_id.clone(),
        AcpSession {
            channel_id,
            busy: false,
            cancel: None,
        },
    );

    info!(session = %session_id, "ACP session resumed");

    // ACP session/resume response is an empty object (no history replay).
    JsonRpcResponse::success(id, json!({}))
}

/// Derive the deterministic `channel_id` (`acp_<uuid>`) from a client-supplied
/// `sessionId` (`sess_<uuid>`). Returns `None` if malformed — the uuid must
/// parse, which keeps a resumed channel inside the `acp_` namespace and rejects
/// forged ids.
fn derive_channel_id(session_id: &str) -> Option<String> {
    let uuid = session_id.strip_prefix("sess_")?;
    Uuid::parse_str(uuid).ok()?;
    Some(format!("acp_{uuid}"))
}

async fn handle_session_prompt(
    state: &Arc<crate::AppState>,
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    id: Value,
    params: Option<&Value>,
    out_tx: &mpsc::UnboundedSender<String>,
) {
    // Extract sessionId and prompt from params
    let (session_id, prompt_text) = match extract_prompt_params(params) {
        Ok(v) => v,
        Err(e) => {
            let resp = JsonRpcResponse::error(id, -32602, e);
            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            return;
        }
    };

    // Cancel signal for this prompt; `session/cancel` fires it to stop the
    // stream gracefully.
    let cancel = Arc::new(tokio::sync::Notify::new());

    // Look up session and acquire busy lock
    let channel_id = {
        let mut guard = sessions.lock().await;
        match guard.get_mut(&session_id) {
            Some(s) => {
                if s.busy {
                    // Reject concurrent prompts on the same session
                    let resp = JsonRpcResponse::error(
                        id,
                        -32001,
                        "Session busy: a prompt is already in progress",
                    );
                    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                    return;
                }
                s.busy = true;
                s.cancel = Some(cancel.clone());
                s.channel_id.clone()
            }
            None => {
                let resp = JsonRpcResponse::error(
                    id,
                    -32602,
                    format!("Unknown session: {session_id}"),
                );
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                return;
            }
        }
    };

    // Create reply channel for this prompt and register it
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<ReplyChunk>();
    if let Some(ref registry) = state.acp_reply_registry {
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(channel_id.clone(), reply_tx);
    }

    // Convert to GatewayEvent and dispatch
    let event = GatewayEvent::new(
        "acp",
        ChannelInfo {
            id: channel_id.clone(),
            channel_type: "dm".into(),
            thread_id: None,
        },
        SenderInfo {
            id: "acp_client".into(),
            name: "acp_client".into(),
            display_name: "ACP Client".into(),
            is_bot: false,
        },
        &prompt_text,
        &format!("acpmsg_{}", Uuid::new_v4()),
        Vec::new(),
    );

    // Send event through the broadcast channel
    match serde_json::to_string(&event) {
        Ok(json) => {
            if state.event_tx.send(json).is_err() {
                // No receivers — agent/core not connected
                warn!("ACP: event_tx send failed — no agent connected");
                let resp = JsonRpcResponse::error(
                    id,
                    -32603,
                    "No agent backend connected",
                );
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                // Release busy flag
                if let Some(s) = sessions.lock().await.get_mut(&session_id) {
                    s.busy = false;
                }
                // Cleanup registry
                if let Some(ref registry) = state.acp_reply_registry {
                    registry.lock().unwrap_or_else(|e| e.into_inner()).remove(&channel_id);
                }
                return;
            }
        }
        Err(e) => {
            warn!("ACP: failed to serialize event: {e}");
            let resp = JsonRpcResponse::error(id, -32603, "Internal error");
            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            if let Some(s) = sessions.lock().await.get_mut(&session_id) {
                s.busy = false;
            }
            return;
        }
    }

    info!(session = %session_id, channel = %channel_id, "ACP: prompt dispatched");

    // Stream replies back as ACP `session/update` notifications.
    let mut sent_len = 0usize;
    let timeout = tokio::time::Duration::from_secs(180);
    let mut stop_reason = "end_turn";
    let mut timed_out = false;

    loop {
        tokio::select! {
            // session/cancel fired — stop gracefully.
            _ = cancel.notified() => {
                stop_reason = "cancelled";
                break;
            }
            recv = tokio::time::timeout(timeout, reply_rx.recv()) => {
                match recv {
                    Ok(Some(ReplyChunk::Text(full_text))) => {
                        // Emit new text as an `agent_message_chunk` update. See
                        // `stream_delta` for the char-boundary safety guarantee.
                        let delta = match stream_delta(sent_len, &full_text) {
                            Some(d) => d,
                            None => continue,
                        };
                        sent_len = full_text.len();

                        let notification = JsonRpcNotification {
                            jsonrpc: "2.0",
                            method: "session/update".into(),
                            params: json!({
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": delta}
                                }
                            }),
                        };
                        let _ = out_tx.send(serde_json::to_string(&notification).unwrap());
                    }
                    Ok(Some(ReplyChunk::Done)) | Ok(None) => break,
                    Err(_) => {
                        warn!(session = %session_id, "ACP: prompt timed out waiting for reply");
                        timed_out = true;
                        break;
                    }
                }
            }
        }
    }

    // Cleanup: remove from registry, release busy flag, clear cancel signal.
    if let Some(ref registry) = state.acp_reply_registry {
        registry.lock().unwrap_or_else(|e| e.into_inner()).remove(&channel_id);
    }
    if let Some(s) = sessions.lock().await.get_mut(&session_id) {
        s.busy = false;
        s.cancel = None;
    }

    // Final response. A backend timeout has no ACP stopReason, so it is an error;
    // otherwise return the turn's PromptResponse { stopReason }.
    let resp = if timed_out {
        JsonRpcResponse::error(id, -32603, "Timed out waiting for agent backend")
    } else {
        JsonRpcResponse::success(id, json!({ "stopReason": stop_reason }))
    };
    let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
}

fn extract_prompt_params(params: Option<&Value>) -> Result<(String, String), String> {
    let params = params.ok_or("Missing params")?;
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("Missing sessionId")?
        .to_string();
    let prompt = params.get("prompt").ok_or("Missing prompt")?;

    // Prompt can be an array of content blocks or a simple string. The base is
    // text-only: an unsupported block type (image / audio / resource / resource_link)
    // is rejected explicitly rather than silently dropped, so the client knows its
    // content was not delivered.
    let text = if let Some(arr) = prompt.as_array() {
        let mut parts: Vec<String> = Vec::with_capacity(arr.len());
        for block in arr {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let t = block
                        .get("text")
                        .and_then(|t| t.as_str())
                        .ok_or("Text content block missing 'text'")?;
                    parts.push(t.to_string());
                }
                Some("resource_link") => {
                    // Baseline ACP content (every agent MUST accept text + resource_link).
                    // We do not fetch the resource (that would be an SSRF risk); the link
                    // reference is passed through as text so the agent can act on it.
                    let uri = block
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .ok_or("resource_link content block missing 'uri'")?;
                    let label = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| block.get("title").and_then(|v| v.as_str()));
                    parts.push(match label {
                        Some(l) => format!("[{l}]({uri})"),
                        None => uri.to_string(),
                    });
                }
                Some(other) => {
                    // Capability-gated variants (image / audio / embedded resource) that
                    // this agent does not advertise in promptCapabilities are rejected
                    // explicitly rather than silently dropped.
                    return Err(format!(
                        "Unsupported prompt content block type '{other}' — this agent advertises no such capability (base accepts text and resource_link)"
                    ));
                }
                None => return Err("Prompt content block missing 'type'".into()),
            }
        }
        parts.join("\n")
    } else if let Some(s) = prompt.as_str() {
        s.to_string()
    } else {
        return Err("Invalid prompt format".into());
    };

    if text.trim().is_empty() {
        return Err("Empty prompt".into());
    }

    Ok((session_id, text))
}

// ---------------------------------------------------------------------------
// Reply handler: called when GatewayReply arrives for an ACP session
// ---------------------------------------------------------------------------

/// Process a GatewayReply destined for an ACP session.
/// Called from the unified bridge's reply dispatch logic.
pub async fn handle_reply(reply: &GatewayReply, registry: &AcpReplyRegistry) {
    let key = reply.channel.id.as_str();
    if !key.starts_with("acp_") {
        return;
    }

    let full_text = reply.content.text.clone();
    // Skip placeholder/draft messages
    if full_text == "…" || full_text == "draft" {
        return;
    }

    let tx = {
        let map = registry.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(key) {
            Some(tx) => tx.clone(),
            None => return,
        }
    };

    match reply.command.as_deref() {
        Some("edit_message") => {
            // Streaming update — send as text snapshot
            if tx.send(ReplyChunk::Text(full_text)).is_err() {
                debug!(channel = key, "ACP reply send failed (client likely disconnected)");
                registry.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
            }
        }
        None | Some("send_message") => {
            // Final message
            let _ = tx.send(ReplyChunk::Text(full_text));
            let _ = tx.send(ReplyChunk::Done);
            registry.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        }
        Some("add_reaction") | Some("remove_reaction") => {
            // Reactions are agent state indicators — could map to notifications later
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Conformance guard — the wire this server hand-rolls MUST validate against the
// generated ACP v1 types (`acp_schema`). Any casing / field-name / shape drift
// (the exact class of bug fixed during the base build: `agentMessageChunk` →
// `agent_message_chunk`, integer `protocolVersion`, snake_case `stopReason`)
// fails these tests. Also pins the generated types as the schema source of truth
// while the payloads stay hand-rolled (per ADR §7: hand-roll the trivial chat
// subset, generate the complex bidirectional surface).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod acp_conformance {
    use crate::adapters::acp_schema as sc;
    use serde_json::{json, Value};

    /// Assert `wire` (a payload this server emits or accepts) deserializes into the
    /// generated ACP type `T`, and that `T`'s serde is a stable fixed point
    /// (serialize→deserialize→serialize is idempotent). No `PartialEq` needed on `T`.
    fn conforms<T>(wire: Value)
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let a: T = serde_json::from_value(wire.clone())
            .unwrap_or_else(|e| panic!("emitted wire is not valid ACP {}: {e}\n  wire={wire}", std::any::type_name::<T>()));
        let v1 = serde_json::to_value(&a).unwrap();
        let b: T = serde_json::from_value(v1.clone()).expect("re-parse of generated form");
        let v2 = serde_json::to_value(&b).unwrap();
        assert_eq!(v1, v2, "ACP serde is not a stable fixed point for {}", std::any::type_name::<T>());
    }

    // --- outbound responses (exact shapes handle_* emit) ---

    #[test]
    fn initialize_response() {
        // mirror of handle_initialize
        conforms::<sc::InitializeResponse>(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "sessionCapabilities": { "resume": {} },
                "promptCapabilities": { "image": false, "audio": false, "embeddedContext": false }
            },
            "agentInfo": { "name": "openab", "title": "OpenAB", "version": "0.0.0" },
            "authMethods": []
        }));
    }

    #[test]
    fn new_session_response() {
        conforms::<sc::NewSessionResponse>(json!({ "sessionId": "sess_00000000-0000-0000-0000-000000000000" }));
    }

    #[test]
    fn resume_session_response() {
        // handle_session_resume returns {}
        conforms::<sc::ResumeSessionResponse>(json!({}));
    }

    #[test]
    fn prompt_response_stop_reasons() {
        // handle_prompt emits end_turn (normal) / cancelled (session/cancel)
        conforms::<sc::PromptResponse>(json!({ "stopReason": "end_turn" }));
        conforms::<sc::PromptResponse>(json!({ "stopReason": "cancelled" }));
    }

    #[test]
    fn session_update_agent_message_chunk() {
        // the streaming notification `params` (session/update)
        conforms::<sc::SessionNotification>(json!({
            "sessionId": "sess_00000000-0000-0000-0000-000000000000",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "PONG 你好 (๑•̀ㅂ•́)و" }
            }
        }));
    }

    // --- inbound requests (params clients send) ---

    #[test]
    fn prompt_request() {
        conforms::<sc::PromptRequest>(json!({
            "sessionId": "sess_00000000-0000-0000-0000-000000000000",
            "prompt": [{ "type": "text", "text": "PING" }]
        }));
    }

    // --- edge cases: emoji / Unicode / boundary strings (round-trip) ---

    // The multi-byte / multi-codepoint cases a naive wire handler mangles:
    // astral-plane emoji, ZWJ sequence, regional-indicator flag, VS16 emoji,
    // astral-plane CJK, and a mixed run.
    const EDGE_TEXT: &[&str] = &[
        "🎉",                     // U+1F389, 4-byte astral emoji
        "👨‍👩‍👧‍👦",                 // ZWJ family (7 codepoints joined by ZWJ)
        "🇹🇼",                     // regional-indicator pair (flag)
        "❤️",                     // U+2764 + U+FE0F (VS16)
        "𠀀",                     // U+20000, astral-plane CJK
        "🎉 你好 (๑•̀ㅂ•́)و ❤️",      // mixed emoji + CJK + kaomoji + VS16
    ];

    #[test]
    fn content_block_emoji_and_unicode() {
        for e in EDGE_TEXT {
            conforms::<sc::ContentBlock>(json!({ "type": "text", "text": e }));
        }
    }

    #[test]
    fn session_update_emoji_chunk() {
        for e in EDGE_TEXT {
            conforms::<sc::SessionNotification>(json!({
                "sessionId": "sess_00000000-0000-0000-0000-000000000000",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": e }
                }
            }));
        }
    }

    #[test]
    fn content_block_boundary_strings() {
        // empty, whitespace/newlines/tabs, JSON-special chars, control chars, and a
        // long string — all must round-trip as plain text.
        let long = "x".repeat(4096);
        for s in [
            "",
            "   \n\t  ",
            "quote:\" backslash:\\ slash:/ braces:{}[]",
            "ctrl:\u{0001}\u{001f} unit-sep",
            long.as_str(),
        ] {
            conforms::<sc::ContentBlock>(json!({ "type": "text", "text": s }));
        }
    }

    #[test]
    fn prompt_response_all_stop_reasons() {
        for sr in ["end_turn", "max_tokens", "max_turn_requests", "refusal", "cancelled"] {
            conforms::<sc::PromptResponse>(json!({ "stopReason": sr }));
        }
    }

    #[test]
    fn prompt_request_multi_block_emoji() {
        conforms::<sc::PromptRequest>(json!({
            "sessionId": "sess_00000000-0000-0000-0000-000000000000",
            "prompt": [
                { "type": "text", "text": "line 1 🎉" },
                { "type": "text", "text": "你好 ❤️" }
            ]
        }));
    }

    // --- JSON-RPC id semantics (F8): omitted id (notification) vs explicit null (request) ---

    #[test]
    fn jsonrpc_id_presence_distinguishes_notification() {
        // serde's `Option<Value>` collapses BOTH omitted id and explicit `id:null` to
        // `None`, so notification detection must use raw key PRESENCE (as the dispatch
        // does), not the deserialized field.
        let notif: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/cancel"}"#).unwrap();
        assert!(notif.get("id").is_none(), "no id member → notification");
        let req_null: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/cancel","id":null}"#).unwrap();
        assert!(req_null.get("id").is_some(), "explicit id:null → request (id member present)");
        let req_num: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"initialize","id":7}"#).unwrap();
        assert_eq!(req_num.get("id"), Some(&json!(7)));
    }

    // --- required-param validation (F9): reject malformed session/new & resume ---

    #[test]
    fn session_param_validation() {
        use super::validate_params;
        // session/new requires { cwd, mcpServers }
        assert!(validate_params::<sc::NewSessionRequest>(Some(&json!({"cwd": "/w", "mcpServers": []}))).is_ok());
        assert!(validate_params::<sc::NewSessionRequest>(Some(&json!({"mcpServers": []}))).is_err(), "missing cwd");
        assert!(validate_params::<sc::NewSessionRequest>(Some(&json!({"cwd": "/w"}))).is_err(), "missing mcpServers");
        assert!(validate_params::<sc::NewSessionRequest>(None).is_err(), "missing params");
        // session/resume requires { sessionId, cwd }
        assert!(validate_params::<sc::ResumeSessionRequest>(Some(&json!({"sessionId": "sess_x", "cwd": "/w", "mcpServers": []}))).is_ok());
        assert!(validate_params::<sc::ResumeSessionRequest>(Some(&json!({"cwd": "/w"}))).is_err(), "missing sessionId");
        assert!(validate_params::<sc::ResumeSessionRequest>(Some(&json!({"sessionId": "sess_x"}))).is_err(), "missing cwd");
    }

    // --- prompt content blocks (F10): unsupported block types rejected, not dropped ---

    #[test]
    fn prompt_content_blocks_baseline_accepted_gated_rejected() {
        use super::extract_prompt_params;
        // text blocks accepted and concatenated
        let (_, text) = extract_prompt_params(Some(&json!({
            "sessionId": "sess_x",
            "prompt": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]
        })))
        .unwrap();
        assert_eq!(text, "a\nb");
        // resource_link is BASELINE — accepted, rendered as a link reference (not fetched)
        let (_, text) = extract_prompt_params(Some(&json!({
            "sessionId": "sess_x",
            "prompt": [
                {"type": "text", "text": "see"},
                {"type": "resource_link", "uri": "file:///x", "name": "X"}
            ]
        })))
        .unwrap();
        assert_eq!(text, "see\n[X](file:///x)");
        // resource_link without a name/title renders the bare uri
        let (_, text) = extract_prompt_params(Some(&json!({
            "sessionId": "sess_x",
            "prompt": [{"type": "resource_link", "uri": "https://e/x"}]
        })))
        .unwrap();
        assert_eq!(text, "https://e/x");
        // resource_link missing its required uri → error
        assert!(extract_prompt_params(Some(&json!({
            "sessionId": "sess_x",
            "prompt": [{"type": "resource_link", "name": "X"}]
        })))
        .is_err());
        // capability-gated variants (image / audio / embedded resource) are rejected,
        // never silently dropped
        assert!(extract_prompt_params(Some(&json!({
            "sessionId": "sess_x",
            "prompt": [{"type": "image", "data": "..", "mimeType": "image/png"}]
        })))
        .is_err());
        // a plain-string prompt still works
        let (_, s) = extract_prompt_params(Some(&json!({"sessionId": "sess_x", "prompt": "hello"}))).unwrap();
        assert_eq!(s, "hello");
    }
}

// ---------------------------------------------------------------------------
// Streaming slicer — the char-boundary-safe incremental delta logic. A multi-byte
// codepoint (emoji, CJK) would be split here if the wire used byte indexing; these
// pin that it never happens.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod acp_streaming {
    use super::stream_delta;

    /// Replay a sequence of full-text snapshots through the exact loop logic
    /// (`stream_delta` + advancing `sent_len`) and return the concatenated deltas.
    fn replay(snapshots: &[&str]) -> String {
        let mut sent = 0usize;
        let mut out = String::new();
        for snap in snapshots {
            if let Some(delta) = stream_delta(sent, snap) {
                out.push_str(delta);
                sent = snap.len();
            }
        }
        out
    }

    #[test]
    fn append_reconstructs_exactly() {
        assert_eq!(replay(&["", "H", "Hi", "Hi ", "Hi there"]), "Hi there");
    }

    #[test]
    fn multibyte_codepoints_never_split() {
        // each snapshot appends a whole multi-byte grapheme; reconstruction is exact
        let snaps = [
            "a",
            "a🎉",
            "a🎉你",
            "a🎉你👨‍👩‍👧‍👦",
            "a🎉你👨‍👩‍👧‍👦🇹🇼",
            "a🎉你👨‍👩‍👧‍👦🇹🇼❤️",
        ];
        assert_eq!(replay(&snaps), *snaps.last().unwrap());
    }

    #[test]
    fn emoji_appears_whole_in_one_delta() {
        // "ab" already sent; next snapshot adds a 4-byte emoji → delta is the whole emoji
        assert_eq!(stream_delta(2, "ab🎉"), Some("🎉"));
    }

    #[test]
    fn mid_codepoint_sent_len_is_skipped_not_panicked() {
        // sent_len inside the 4-byte emoji (a non-append rewrite) → None, never a panic
        assert_eq!(stream_delta(1, "🎉"), None);
        assert_eq!(stream_delta(2, "🎉"), None);
        assert_eq!(stream_delta(3, "🎉"), None);
    }

    #[test]
    fn no_new_text_returns_none() {
        assert_eq!(stream_delta(5, "hello"), None); // sent == len
        assert_eq!(stream_delta(9, "hello"), None); // sent beyond len (shrink/rewrite)
    }

    #[test]
    fn empty_snapshot_returns_none() {
        assert_eq!(stream_delta(0, ""), None);
    }
}

// ---------------------------------------------------------------------------
// Handler-level tests — call the real handlers (not just literal round-trips) and
// assert their actual output + side effects.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod acp_handlers {
    use super::{
        handle_initialize, handle_session_new, handle_session_resume, AcpSession, JsonRpcRequest,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    fn new_sessions() -> Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>> {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    fn init_req(params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "initialize".into(),
            id: Some(json!(1)),
            params,
        }
    }

    #[test]
    fn initialize_returns_conformant_capabilities() {
        let v = serde_json::to_value(handle_initialize(&init_req(Some(json!({"protocolVersion": 1}))))).unwrap();
        assert_eq!(v["id"], json!(1));
        let result = &v["result"];
        assert_eq!(result["protocolVersion"], json!(1));
        assert_eq!(result["agentCapabilities"]["loadSession"], json!(false));
        assert!(result["agentCapabilities"]["sessionCapabilities"]["resume"].is_object());
        assert!(result["authMethods"].is_array());
    }

    #[test]
    fn initialize_negotiates_version_and_rejects_bad() {
        // a higher client version negotiates down to ours (1)
        let v = serde_json::to_value(handle_initialize(&init_req(Some(json!({"protocolVersion": 5}))))).unwrap();
        assert_eq!(v["result"]["protocolVersion"], json!(1));
        // version 0 is below our minimum → -32602
        let v = serde_json::to_value(handle_initialize(&init_req(Some(json!({"protocolVersion": 0}))))).unwrap();
        assert_eq!(v["error"]["code"], json!(-32602));
        // missing protocolVersion → -32602
        let v = serde_json::to_value(handle_initialize(&init_req(Some(json!({}))))).unwrap();
        assert_eq!(v["error"]["code"], json!(-32602));
        // missing params → -32602
        let v = serde_json::to_value(handle_initialize(&init_req(None))).unwrap();
        assert_eq!(v["error"]["code"], json!(-32602));
    }

    #[tokio::test]
    async fn session_new_mints_and_stores_a_session() {
        let sessions = new_sessions();
        let v = serde_json::to_value(handle_session_new(&sessions, json!(2)).await).unwrap();
        let sid = v["result"]["sessionId"].as_str().unwrap();
        assert!(sid.starts_with("sess_"), "sessionId must be sess_<uuid>: {sid}");
        assert!(sessions.lock().await.contains_key(sid), "session must be stored");
    }

    #[tokio::test]
    async fn session_resume_valid_stores_and_invalid_errors() {
        let sessions = new_sessions();
        // valid sess_<uuid> → {} and the session is (re)stored
        let sid = format!("sess_{}", Uuid::new_v4());
        let params = json!({"sessionId": sid, "cwd": "/w", "mcpServers": []});
        let v = serde_json::to_value(handle_session_resume(&sessions, json!(3), Some(&params)).await)
            .unwrap();
        assert_eq!(v["result"], json!({}));
        assert!(sessions.lock().await.contains_key(&sid));
        // malformed sessionId shape → -32602
        let bad = json!({"sessionId": "not-a-session", "cwd": "/w", "mcpServers": []});
        let v = serde_json::to_value(handle_session_resume(&sessions, json!(4), Some(&bad)).await)
            .unwrap();
        assert_eq!(v["error"]["code"], json!(-32602));
        // missing sessionId → -32602
        let v = serde_json::to_value(
            handle_session_resume(&sessions, json!(5), Some(&json!({"cwd": "/w"}))).await,
        )
        .unwrap();
        assert_eq!(v["error"]["code"], json!(-32602));
    }
}

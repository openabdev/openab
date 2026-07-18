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

/// Whether ACP frame tracing is on (`OPENAB_ACP_TRACE=1|true`). When set, every
/// JSON-RPC frame on the upstream client↔gateway hop is logged in both directions
/// (`dir="in"` / `dir="out"`). Off by default; enable to capture real traffic — e.g.
/// to validate the generated-type round-trip against what clients/agents actually emit.
pub(crate) fn acp_trace_enabled() -> bool {
    std::env::var("OPENAB_ACP_TRACE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
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
                info!(connection = %send_conn, dir = "out", frame = %msg, "ACP frame");
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

        if trace {
            info!(connection = %connection_id, dir = "in", frame = %text, "ACP frame");
        }

        let req: JsonRpcRequest = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = JsonRpcResponse::error(
                    Value::Null,
                    -32700,
                    format!("Parse error: {e}"),
                );
                let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
                continue;
            }
        };

        // Validate JSON-RPC version (spec requires "2.0")
        if req.jsonrpc != "2.0" {
            let err_resp = JsonRpcResponse::error(
                req.id.clone().unwrap_or(Value::Null),
                -32600,
                "Invalid Request: jsonrpc must be \"2.0\"",
            );
            let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
            continue;
        }

        let id = req.id.clone().unwrap_or(Value::Null);

        match req.method.as_str() {
            "initialize" => {
                let resp = handle_initialize(&req);
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                initialized = true;
            }
            "session/new" => {
                if !initialized {
                    let resp = JsonRpcResponse::error(id, -32002, "Not initialized");
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
                let resp = handle_session_resume(&sessions, id.clone(), req.params.as_ref()).await;
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            }
            "session/prompt" => {
                if !initialized {
                    let resp = JsonRpcResponse::error(id, -32002, "Not initialized");
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
            }
            _ => {
                let resp = JsonRpcResponse::error(
                    id,
                    -32601,
                    format!("Method not found: {}", req.method),
                );
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
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
    // ACP initialize response. protocolVersion is an integer MAJOR version.
    // We advertise `sessionCapabilities.resume` (we support session/resume) but
    // NOT `loadSession` — the gateway cannot replay conversation history to the
    // client (it lives inside the downstream agent CLI), so load is unsupported.
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
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
                        // Emit new text as an `agent_message_chunk` update.
                        // Slice via `get` (not byte-index `[..]`) so a `sent_len`
                        // that lands mid-codepoint — possible with CJK / 顏文字 /
                        // emoji if a snapshot is ever non-append — skips this frame
                        // instead of panicking; the next snapshot re-covers it.
                        let delta = match full_text.get(sent_len..) {
                            Some(d) if !d.is_empty() => d,
                            _ => continue,
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

    // Prompt can be an array of content blocks or a simple string
    let text = if let Some(arr) = prompt.as_array() {
        arr.iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
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

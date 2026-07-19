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
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
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

/// WebSocket subprotocol prefix that carries the bearer token from a browser client
/// (browsers cannot set an `Authorization` header on a WS handshake, but they CAN offer
/// subprotocols via `new WebSocket(url, protocols)`). The client offers
/// `Sec-WebSocket-Protocol: openab.bearer.<token>, acp.v1`; the server extracts the token
/// and echoes the real `acp.v1` subprotocol so the handshake completes. This keeps the
/// token OUT of the URL — the de facto browser-WS bearer pattern (as used by the
/// Kubernetes API server). Non-browser clients should prefer `Authorization: Bearer`.
const BEARER_SUBPROTOCOL_PREFIX: &str = "openab.bearer.";
/// The real ACP subprotocol echoed back on a successful upgrade.
const ACP_SUBPROTOCOL: &str = "acp.v1";

/// Extract the bearer token from a `Sec-WebSocket-Protocol` offer (the
/// `openab.bearer.<token>` entry), if present.
fn subprotocol_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|list| {
            list.split(',')
                .map(str::trim)
                .find_map(|p| p.strip_prefix(BEARER_SUBPROTOCOL_PREFIX))
        })
}

/// Extract the transport bearer key from a WS upgrade request, in priority order:
///   1. `Authorization: Bearer <token>` — non-browser clients (cleanest).
///   2. `Sec-WebSocket-Protocol: openab.bearer.<token>, acp.v1` — browsers (keeps the token
///      out of the URL; the de facto browser-WS bearer pattern).
///
/// The legacy `?token=<token>` query fallback was removed (R17-F2): it leaks the key into
/// URLs / access logs / history, so only these two header-borne sources carry the bearer.
fn ws_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| subprotocol_token(headers))
}

/// RFC 6455 subprotocol values must be RFC 7230 `token`s. `tchar` = ALPHA / DIGIT /
/// `!#$%&'*+-.^_`|~`. A key with any char outside this set (e.g. base64 `/` or `=`)
/// cannot ride the `openab.bearer.<token>` subprotocol on a strict browser handshake.
fn is_ws_subprotocol_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

// ---------------------------------------------------------------------------
// ACP Configuration
// ---------------------------------------------------------------------------

pub struct AcpConfig {
    pub auth_key: Option<String>,
    /// Browser `Origin`s allowed to drive `/acp` in keyless loopback mode (from
    /// `OPENAB_ACP_ALLOWED_ORIGINS`, comma-separated). Empty by default → every
    /// browser-set `Origin` is rejected; non-browser clients (no `Origin`) are unaffected.
    pub allowed_origins: Vec<String>,
}

impl AcpConfig {
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("OPENAB_ACP_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        // Treat an empty value as unset (an empty string is not a usable key).
        let auth_key = std::env::var("OPENAB_ACP_AUTH_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        match auth_key {
            None => warn!(
                "OPENAB_ACP_AUTH_KEY not set — /acp is only served on a loopback bind; a \
                 non-loopback bind will refuse to mount it (set a key to expose it)"
            ),
            Some(ref key) if !key.bytes().all(is_ws_subprotocol_token_char) => warn!(
                "OPENAB_ACP_AUTH_KEY contains characters outside the WebSocket subprotocol \
                 token set (RFC 6455) — a browser passing it via `Sec-WebSocket-Protocol: \
                 openab.bearer.<token>` may fail the handshake (base64 `/` and `=` padding \
                 are the usual offenders). Prefer a key in [A-Za-z0-9._~+-]; the \
                 `Authorization: Bearer` and `?token=` paths are unaffected"
            ),
            Some(_) => {}
        }
        // Browser-origin allowlist for keyless loopback mode. A WS handshake bypasses the
        // browser same-origin policy, so without this any web page could drive a keyless
        // `ws://127.0.0.1/acp`. Comma-separated; blanks trimmed. Default empty blocks all
        // browser origins (a non-browser client sends no `Origin` and is unaffected).
        let allowed_origins = std::env::var("OPENAB_ACP_ALLOWED_ORIGINS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(Self {
            auth_key,
            allowed_origins,
        })
    }
}

/// Whether a **keyless-mode** WS upgrade may proceed given its `Origin` header. WS
/// handshakes are exempt from the browser same-origin policy, so on a keyless loopback
/// bind any web page could otherwise drive `/acp`. A request with no `Origin` (a
/// non-browser client) is allowed; a browser-set `Origin` must be explicitly allowlisted
/// via `OPENAB_ACP_ALLOWED_ORIGINS` (default empty → every browser origin blocked). Keyed
/// binds authenticate via the bearer key and never reach this check.
fn acp_origin_ok(origin: Option<&str>, allowed_origins: &[String]) -> bool {
    match origin {
        None => true,
        Some(o) => allowed_origins.iter().any(|a| a == o),
    }
}

/// Whether the listen address binds a loopback interface (`127.0.0.0/8`, `::1`, or
/// `localhost`). An unknown / unparseable host is treated as non-loopback (fail safe).
fn bind_is_loopback(listen_addr: &str) -> bool {
    let host = listen_addr
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(listen_addr)
        .trim_matches(|c| c == '[' || c == ']'); // strip IPv6 brackets
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Whether `/acp` may be mounted for the given auth key and bind address. A non-empty
/// transport key always suffices. Without a key, fail-open is permitted ONLY on a
/// loopback bind; any non-loopback bind (`0.0.0.0`, a LAN IP, a LoadBalancer) requires
/// `OPENAB_ACP_AUTH_KEY` so an unauthenticated agent endpoint is never exposed to the
/// network. Returns `Err(reason)` when the endpoint must not be mounted.
pub fn acp_auth_ok_for_bind(auth_key: Option<&str>, listen_addr: &str) -> Result<(), String> {
    if auth_key.map(|k| !k.is_empty()).unwrap_or(false) {
        return Ok(());
    }
    if bind_is_loopback(listen_addr) {
        return Ok(());
    }
    Err(format!(
        "OPENAB_ACP_AUTH_KEY is required to serve /acp on a non-loopback address \
         ({listen_addr}); refusing to expose an unauthenticated agent endpoint"
    ))
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
    /// Client-declared MCP-over-ACP servers (the RFD `{"type":"acp"}` mcpServers entries):
    /// the browser extension serves its MCP tools over this same /acp WS. Recorded so the
    /// gateway can later `mcp/connect` to them (T5.3). Unused until that wiring lands.
    #[allow(dead_code)]
    acp_mcp_servers: Vec<AcpMcpServer>,
}

/// A client-declared MCP-over-ACP server (the RFD `"type":"acp"` `mcpServers` entry). Not in
/// the generated schema (the RFD is a proposal), so parsed from raw params.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
struct AcpMcpServer {
    id: String,
    name: String,
}

/// Extract the `"type":"acp"` entries from a `session/new` / `session/resume` params
/// `mcpServers` array. http/sse/stdio servers are ignored here — the agent connects to those
/// itself; only the `acp`-transport ones are tunnelled over this WS.
fn parse_acp_mcp_servers(params: Option<&Value>) -> Vec<AcpMcpServer> {
    params
        .and_then(|p| p.get("mcpServers"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|e| e.get("type").and_then(Value::as_str) == Some("acp"))
                .filter_map(|e| {
                    Some(AcpMcpServer {
                        id: e.get("id").and_then(Value::as_str)?.to_string(),
                        name: e.get("name").and_then(Value::as_str)?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub enum ReplyChunk {
    /// Incremental text snapshot (full text so far)
    Text(String),
    /// Agent finished responding
    Done,
}

/// One active turn's reply sink plus the originating `GatewayEvent` id used to fence
/// stale replies. After a prompt times out / is cancelled, the next prompt on the same
/// session reuses the same deterministic `channel_id`; a late reply from the superseded
/// turn carries that turn's `evt_<uuid>` in `GatewayReply.reply_to`, so matching it
/// against `turn_id` drops it instead of mis-delivering into the new prompt's stream.
pub struct ReplySink {
    /// Originating `GatewayEvent.event_id` (`evt_<uuid>`), round-tripped as `reply_to`.
    pub turn_id: String,
    pub tx: mpsc::UnboundedSender<ReplyChunk>,
}

/// Registry of active ACP sessions: channel_id → reply sink.
/// Uses std::sync::Mutex because all operations are fast CPU-bound
/// (insert/remove/get) and never hold the lock across .await.
pub type AcpReplyRegistry = Arc<std::sync::Mutex<HashMap<String, ReplySink>>>;

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

/// A SERVER-INITIATED JSON-RPC request (the agent→client REQUEST direction, T1). The base
/// only ever *received* requests, so `JsonRpcRequest` is deserialize-only; this is the
/// outbound counterpart used by `send_request`. Wired to a caller by T1.4 (core↔gateway
/// bridge) / the MCP-over-ACP tunnel; landed ahead of its caller as ready infrastructure.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct JsonRpcRequestOut {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

// --- MCP-over-ACP tunnel frames (T4) ------------------------------------------
// The official MCP-over-ACP RFD (agentclientprotocol.com/rfds/mcp-over-acp) tunnels MCP
// over the /acp WS with three methods: mcp/connect → connectionId, then mcp/message (the
// inner MCP method/params FLATTENED into the params, correlated by the OUTER ACP id — the
// inner MCP id is not carried), and mcp/disconnect. These types are NOT in the generated
// `acp_schema` (the RFD is a proposal, not the stable v1 schema), so they are hand-rolled
// here. The extension is the MCP server and assigns `connectionId`; the gateway (agent side
// of the upstream hop) is the connector and issues connect/message/disconnect.

/// `mcp/connect` params — `acpId` matches the `id` of the client's `session/new`
/// `mcpServers` entry with `"type":"acp"`.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct McpConnectParams {
    #[serde(rename = "acpId")]
    acp_id: String,
}

/// `mcp/connect` result — the client-assigned connection handle.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct McpConnectResult {
    #[serde(rename = "connectionId")]
    connection_id: String,
}

/// `mcp/message` params — the inner MCP `method`/`params` are flattened in (no inner id);
/// `connectionId` selects the tunnelled MCP connection.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct McpMessageParams {
    #[serde(rename = "connectionId")]
    connection_id: String,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// `mcp/disconnect` params.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct McpDisconnectParams {
    #[serde(rename = "connectionId")]
    connection_id: String,
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
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    // Bearer key from a header-borne source only (Authorization or the WS subprotocol);
    // the legacy `?token=` query fallback was dropped in R17-F2. See `ws_bearer_token`.
    let token = ws_bearer_token(&headers);

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
    } else {
        // Keyless loopback mode: the bearer check above is skipped, so a browser could
        // reach us cross-origin (WS handshakes bypass the same-origin policy). Reject a
        // browser-set `Origin` that isn't allowlisted; a non-browser client (no `Origin`)
        // is allowed.
        let origin = headers.get("origin").and_then(|v| v.to_str().ok());
        let allowed = state
            .acp
            .as_ref()
            .map(|c| c.allowed_origins.as_slice())
            .unwrap_or(&[]);
        if !acp_origin_ok(origin, allowed) {
            warn!(
                "ACP WebSocket rejected: browser Origin {:?} not in OPENAB_ACP_ALLOWED_ORIGINS \
                 (keyless loopback mode)",
                origin
            );
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    // Echo the `acp.v1` subprotocol so a browser that offered it (alongside its
    // `openab.bearer.<token>` entry) completes the handshake. Clients that offer no
    // subprotocol are unaffected.
    ws.protocols([ACP_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_acp_connection(state, socket))
}

// ---------------------------------------------------------------------------
// ACP Connection handler
// ---------------------------------------------------------------------------

/// Route an inbound client *response* (an id-bearing frame with `result`/`error` and NO
/// `method`) to the `send_request` awaiter registered under its id. Returns `true` when the
/// frame was a response we consumed (so the caller must stop dispatching it as a request).
/// Mirrors the client-side correlation in `openab-core/src/acp/connection.rs`.
async fn route_client_response(
    pending: &Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    raw: &Value,
) -> bool {
    let has_method = raw.get("method").is_some();
    let looks_like_response = raw.get("result").is_some() || raw.get("error").is_some();
    if has_method || !looks_like_response {
        return false;
    }
    let Some(id) = raw.get("id").and_then(Value::as_u64) else {
        return false;
    };
    if let Some(tx) = pending.lock().await.remove(&id) {
        let _ = tx.send(raw.clone());
    } else {
        warn!(id, "acp: client response with no matching pending request");
    }
    true
}

/// Send a server-initiated JSON-RPC request to the connected ACP client and await its
/// response (the agent→client REQUEST direction, T1). Mints an id, registers a oneshot in
/// `pending`, writes the frame via the outbound channel, then awaits the correlated response
/// (resolved by `route_client_response`) with a timeout. Wired to a caller by T1.4 (the
/// core↔gateway MCP-over-ACP bridge); landed ahead of its caller as ready infrastructure.
#[allow(dead_code)]
async fn send_request(
    out_tx: &mpsc::UnboundedSender<String>,
    pending: &Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: &AtomicU64,
    method: impl Into<String>,
    params: Value,
    timeout_secs: u64,
) -> Result<Value, String> {
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    let frame = JsonRpcRequestOut {
        jsonrpc: "2.0",
        id,
        method: method.into(),
        params,
    };
    if out_tx.send(serde_json::to_string(&frame).unwrap()).is_err() {
        pending.lock().await.remove(&id);
        return Err("connection closed".into());
    }
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err("connection closed before response".into()),
        Err(_) => {
            pending.lock().await.remove(&id);
            Err("request timed out".into())
        }
    }
}

/// Extract the `result` from a JSON-RPC response frame, mapping an `error` member to `Err`.
/// `send_request` yields the whole response frame; the tunnel helpers want just the payload.
#[allow(dead_code)]
fn frame_result(frame: Value) -> Result<Value, String> {
    if let Some(err) = frame.get("error") {
        return Err(format!("remote error: {err}"));
    }
    Ok(frame.get("result").cloned().unwrap_or(Value::Null))
}

/// `mcp/connect` (T4): open a tunnelled MCP connection to the client-provided (`"type":"acp"`)
/// MCP server identified by `acp_id`; returns the client-assigned `connectionId`.
#[allow(dead_code)]
async fn mcp_connect(
    out_tx: &mpsc::UnboundedSender<String>,
    pending: &Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: &AtomicU64,
    acp_id: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    let params = serde_json::to_value(McpConnectParams {
        acp_id: acp_id.to_string(),
    })
    .unwrap();
    let frame = send_request(out_tx, pending, next_id, "mcp/connect", params, timeout_secs).await?;
    let result: McpConnectResult = serde_json::from_value(frame_result(frame)?)
        .map_err(|e| format!("mcp/connect: malformed result: {e}"))?;
    Ok(result.connection_id)
}

/// `mcp/message` REQUEST (T4): tunnel an inner MCP request over `connection_id`; returns the
/// inner MCP result payload (the outer ACP id does the correlation; the inner MCP id is not
/// carried on the wire).
#[allow(dead_code)]
async fn mcp_message_request(
    out_tx: &mpsc::UnboundedSender<String>,
    pending: &Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: &AtomicU64,
    connection_id: &str,
    method: &str,
    params: Option<Value>,
    timeout_secs: u64,
) -> Result<Value, String> {
    let msg = serde_json::to_value(McpMessageParams {
        connection_id: connection_id.to_string(),
        method: method.to_string(),
        params,
    })
    .unwrap();
    let frame = send_request(out_tx, pending, next_id, "mcp/message", msg, timeout_secs).await?;
    frame_result(frame)
}

/// `mcp/disconnect` (T4): close a tunnelled MCP connection.
#[allow(dead_code)]
async fn mcp_disconnect(
    out_tx: &mpsc::UnboundedSender<String>,
    pending: &Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: &AtomicU64,
    connection_id: &str,
    timeout_secs: u64,
) -> Result<(), String> {
    let params = serde_json::to_value(McpDisconnectParams {
        connection_id: connection_id.to_string(),
    })
    .unwrap();
    send_request(out_tx, pending, next_id, "mcp/disconnect", params, timeout_secs)
        .await
        .map(|_| ())
}

async fn handle_acp_connection(state: Arc<crate::AppState>, socket: WebSocket) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let connection_id = format!("acp_conn_{}", Uuid::new_v4());

    info!(connection = %connection_id, "ACP client connected");

    // Frame tracing (OPENAB_ACP_TRACE) — read once per connection.
    let trace = acp_trace_enabled();

    // Session state for this connection
    let sessions: Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    // Pending server-initiated requests (T1): id → oneshot for the client's response. The
    // base had only client→server requests + server→client notifications; the MCP-over-ACP
    // tunnel adds the server→client REQUEST direction, correlated through this map by
    // `route_client_response` (inbound) and `send_request` (outbound, wired in T1.4).
    let pending_requests: Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
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

        // Bound inbound frame size before parsing. An oversized frame can't be parsed,
        // so we can't tell request from notification or recover its id — do NOT fabricate
        // a JSON-RPC response (which would violate notification silence). Treat it as a
        // transport-level violation: log and close the connection.
        if text.len() > MAX_FRAME_BYTES {
            warn!(
                connection = %connection_id,
                bytes = text.len(),
                max = MAX_FRAME_BYTES,
                "ACP frame too large; closing connection"
            );
            break;
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

        // JSON-RPC: `id`, when present, MUST be a string, number, or null — never an
        // object, array, or boolean. Reject a wrong-typed id as an Invalid Request.
        if let Some(id) = raw.get("id") {
            if !(id.is_string() || id.is_number() || id.is_null()) {
                let err_resp = JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    "Invalid Request: id must be a string, number, or null",
                );
                let _ = out_tx.send(serde_json::to_string(&err_resp).unwrap());
                continue;
            }
        }

        // A client *response* to a server-initiated request (T1): id present, no `method`,
        // carries `result`/`error`. Route it to the waiting `send_request` and stop — it is
        // neither a client request nor a notification. Gated on `!is_notification` (a
        // notification never carries an id, so it can never be a response), keeping the
        // existing notification/request handling below untouched.
        if !is_notification && route_client_response(&pending_requests, &raw).await {
            continue;
        }

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
                let acp_mcp_servers = parse_acp_mcp_servers(req.params.as_ref());
                let resp = handle_session_new(&sessions, id.clone(), acp_mcp_servers).await;
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
                // Reserve this prompt's cancel state SYNCHRONOUSLY here — before spawning the
                // async handler — so a `session/cancel` arriving on the very next frame finds
                // `s.cancel` already installed. The read loop is sequential; installing the cancel
                // inside the spawned task (as it was) left a window where an immediate cancel read
                // `s.cancel == None` and was dropped, so the prompt ran uncancelled (R16-F1).
                // Unknown-session / busy are rejected here (moved out of the handler).
                let session_id = match req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("sessionId"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) => s.to_string(),
                    None => {
                        let resp = JsonRpcResponse::error(id, -32602, "Missing sessionId");
                        let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                        continue;
                    }
                };
                let cancel = Arc::new(tokio::sync::Notify::new());
                {
                    let mut guard = sessions.lock().await;
                    match guard.get_mut(&session_id) {
                        None => {
                            drop(guard);
                            let resp = JsonRpcResponse::error(
                                id,
                                -32602,
                                format!("Unknown session: {session_id}"),
                            );
                            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                            continue;
                        }
                        Some(s) if s.busy => {
                            drop(guard);
                            let resp = JsonRpcResponse::error(
                                id,
                                -32001,
                                "Session busy: a prompt is already in progress",
                            );
                            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
                            continue;
                        }
                        Some(s) => {
                            s.busy = true;
                            s.cancel = Some(cancel.clone());
                        }
                    }
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
                        session_id,
                        cancel,
                    )
                    .await;
                });
                prompt_tasks.push(handle);
            }
            "session/cancel" => {
                // Notification form fires the cancel signal (no response); a request-shaped
                // cancel is rejected -32600 rather than acked with an empty success (R17-F3c).
                if let Some(resp) =
                    handle_session_cancel(&sessions, id, req.params.as_ref(), is_notification).await
                {
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

    // Drain any in-flight server-initiated requests: dropping each oneshot sender makes the
    // corresponding `send_request` awaiter resolve to "connection closed before response"
    // rather than hang until timeout. Mirrors the close-drain in openab-core connection.rs.
    for (_id, tx) in pending_requests.lock().await.drain() {
        drop(tx);
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
    acp_mcp_servers: Vec<AcpMcpServer>,
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
            acp_mcp_servers,
        },
    );

    // Downgraded from info! — sessionId is a resume capability; keep it out of normal logs (F12).
    debug!(session = %session_id, "ACP session created");

    // ACP session/new response is just { sessionId }. Constructed from the generated
    // NewSessionResponse (T2.1) so the wire shape is type-checked against acp_schema; the
    // optional fields skip-serialize, giving the same { "sessionId": ... } wire.
    let resp = crate::adapters::acp_schema::NewSessionResponse {
        session_id: crate::adapters::acp_schema::SessionId(session_id),
        config_options: None,
        meta: None,
        modes: None,
    };
    JsonRpcResponse::success(id, serde_json::to_value(&resp).unwrap())
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

    let mut guard = sessions.lock().await;
    // Same per-connection cap as session/new: resume must not be an unbounded insert
    // path (a client can mint unlimited valid `sess_<uuid>`). An already-present key is
    // exempt so re-resuming an existing session stays idempotent.
    if !guard.contains_key(&session_id) && guard.len() >= MAX_SESSIONS_PER_CONNECTION {
        return JsonRpcResponse::error(
            id,
            ACP_OVERLOADED,
            format!("Too many sessions on this connection (max {MAX_SESSIONS_PER_CONNECTION})"),
        );
    }
    // R16-F2: refuse to resume a session that currently has a prompt in flight. The insert
    // below unconditionally rewrites AcpSession{busy:false,cancel:None}, which would drop the
    // active turn's cancel handle — and then that turn's cleanup would clobber the resumed
    // state / registry entry, losing its replies. A busy session is already live on this
    // connection, so reject deterministically instead of stomping it.
    if guard.get(&session_id).is_some_and(|s| s.busy) {
        return JsonRpcResponse::error(
            id,
            -32001,
            "Session busy: a prompt is in progress; cannot resume",
        );
    }
    guard.insert(
        session_id.clone(),
        AcpSession {
            channel_id,
            busy: false,
            cancel: None,
            // The client re-presents its mcpServers on resume; re-record the acp ones.
            acp_mcp_servers: parse_acp_mcp_servers(params),
        },
    );
    drop(guard);

    debug!(session = %session_id, "ACP session resumed");

    // ACP session/resume response is an empty object (no history replay) — the generated
    // ResumeSessionResponse default serializes to {} (T2.1, type-checked against acp_schema).
    let resp = crate::adapters::acp_schema::ResumeSessionResponse::default();
    JsonRpcResponse::success(id, serde_json::to_value(&resp).unwrap())
}

/// Handle `session/cancel`. Per ACP it is a one-way NOTIFICATION: the notification form
/// (no `id`) fires the session's cancel signal — the in-flight prompt observes it, cleans
/// up, and returns `stopReason:"cancelled"` to the prompt's own request id — and produces
/// no response (`None`). A request-shaped cancel (with an `id`) is a protocol violation: it
/// is rejected with -32600 invalid request and does NOT fire the signal, rather than being
/// acknowledged with an empty success frame (R17-F3c).
async fn handle_session_cancel(
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    id: Value,
    params: Option<&Value>,
    is_notification: bool,
) -> Option<JsonRpcResponse> {
    if !is_notification {
        return Some(JsonRpcResponse::error(
            id,
            -32600,
            "session/cancel is a notification and must not carry an id",
        ));
    }
    let sess_key = params.and_then(|p| p.get("sessionId")).and_then(|v| v.as_str());
    if let Some(k) = sess_key {
        let notify = sessions.lock().await.get(k).and_then(|s| s.cancel.clone());
        if let Some(n) = notify {
            n.notify_one();
        }
    }
    None
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

/// Release a prompt reservation: clear `busy` and drop the cancel handle. Called on every
/// early return once the read loop has reserved the session (R16-F1).
async fn release_prompt(
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    session_id: &str,
) {
    if let Some(s) = sessions.lock().await.get_mut(session_id) {
        s.busy = false;
        s.cancel = None;
    }
}

async fn handle_session_prompt(
    state: &Arc<crate::AppState>,
    sessions: &Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>>,
    id: Value,
    params: Option<&Value>,
    out_tx: &mpsc::UnboundedSender<String>,
    // The caller (read loop) already reserved this session SYNCHRONOUSLY: `busy = true` and
    // `cancel` installed under the session lock (R16-F1). This task owns releasing it on return.
    session_id: String,
    cancel: Arc<tokio::sync::Notify>,
) {
    // sessionId was validated + reserved by the caller; only the prompt body can still be bad.
    let prompt_text = match extract_prompt_params(params) {
        Ok((_sid, text)) => text,
        Err(e) => {
            let resp = JsonRpcResponse::error(id, -32602, e);
            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            release_prompt(sessions, &session_id).await;
            return;
        }
    };

    // The session was reserved a moment ago under the lock; just read its channel_id.
    let channel_id = match sessions.lock().await.get(&session_id) {
        Some(s) => s.channel_id.clone(),
        None => {
            let resp =
                JsonRpcResponse::error(id, -32602, format!("Unknown session: {session_id}"));
            let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            release_prompt(sessions, &session_id).await;
            return;
        }
    };

    // Convert to GatewayEvent and dispatch. Build it first so its `event_id` can fence
    // this turn's replies (round-tripped as `GatewayReply.reply_to`).
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
    let turn_id = event.event_id.clone();

    // Create reply channel for this prompt and register it, keyed by channel_id with the
    // turn's event id so `handle_reply` can drop a stale reply after timeout/cancel reuse.
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<ReplyChunk>();
    if let Some(ref registry) = state.acp_reply_registry {
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(channel_id.clone(), ReplySink { turn_id, tx: reply_tx });
    }

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
                release_prompt(sessions, &session_id).await;
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
            release_prompt(sessions, &session_id).await;
            return;
        }
    }

    debug!(session = %session_id, channel = %channel_id, "ACP: prompt dispatched");

    // Stream replies back as ACP `session/update` notifications.
    let mut sent_len = 0usize;
    let timeout = tokio::time::Duration::from_secs(180);
    // Typed StopReason (T2.1) so the final PromptResponse is constructed from acp_schema.
    let mut stop_reason = crate::adapters::acp_schema::StopReason::EndTurn;
    let mut timed_out = false;

    loop {
        tokio::select! {
            // session/cancel fired — stop gracefully.
            _ = cancel.notified() => {
                stop_reason = crate::adapters::acp_schema::StopReason::Cancelled;
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
    // INVARIANT (R16-F1/F2): this unconditional remove/reset is safe ONLY because no newer
    // turn can exist on this `session_id` while this one runs — both entry points that would
    // start one are busy-gated (`session/prompt` and `session/resume` reject with -32001 when
    // `s.busy`). If that gating is ever relaxed (e.g. multi-turn-per-session), this must become
    // turn/owner-aware (compare the active `turn_id` before `remove`) or it will clobber the
    // newer turn's sink. Cross-connection same-session races remain an accepted residual (F5).
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
        // T2.1: construct the typed PromptResponse; serializes to { "stopReason": ... }.
        let pr = crate::adapters::acp_schema::PromptResponse {
            stop_reason,
            meta: None,
        };
        JsonRpcResponse::success(id, serde_json::to_value(&pr).unwrap())
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

    // Per the ACP schema the generated `PromptRequest.prompt` is `[ContentBlock]`; a plain
    // string (or any non-array) is non-conformant and rejected below (-32602), never
    // leniently coerced. The base is text-only: an unsupported block type (image / audio /
    // resource / resource_link) is rejected explicitly rather than silently dropped, so the
    // client knows its content was not delivered.
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
                    //
                    // The generated `ResourceLink` requires BOTH `name` and `uri` (R17-F3b);
                    // an incomplete link is rejected (-32602) rather than silently rendered.
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or("resource_link content block missing required 'name'")?;
                    let uri = block
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .ok_or("resource_link content block missing required 'uri'")?;
                    // Render as a Markdown link reference using the required `name` (matches
                    // the prior name-preferred labelling; the bare-uri fallback is gone since
                    // `name` is now mandatory).
                    parts.push(format!("[{name}]({uri})"));
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
    } else {
        // A plain-string `prompt` (or any non-array) does not match the generated
        // `PromptRequest.prompt: [ContentBlock]` shape → -32602 invalid params.
        return Err("Invalid prompt: 'prompt' must be an array of content blocks".into());
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
            // Fence stale replies: after a timeout/cancel the channel_id is reused by the
            // next turn. A late reply carries the previous turn's `evt_<uuid>` in
            // `reply_to`; deliver only when it matches the active turn. Empty `reply_to`
            // (no origin id) fails open so legit traffic is never dropped.
            Some(sink) if reply.reply_to.is_empty() || reply.reply_to == sink.turn_id => {
                sink.tx.clone()
            }
            Some(_) => {
                debug!(channel = key, "ACP dropping stale reply from a superseded turn");
                return;
            }
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
        // R17-F3b — `ResourceLink` requires `name`; a link missing it is rejected (-32602),
        // no longer rendered as a bare uri.
        assert!(
            extract_prompt_params(Some(&json!({
                "sessionId": "sess_x",
                "prompt": [{"type": "resource_link", "uri": "https://e/x"}]
            })))
            .is_err(),
            "resource_link missing required 'name' must be rejected"
        );
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
        // R17-F3a — a plain-string prompt is non-conformant (schema requires
        // `prompt: [ContentBlock]`) → rejected, surfaced as -32602 at the call site.
        assert!(
            extract_prompt_params(Some(&json!({"sessionId": "sess_x", "prompt": "hello"}))).is_err(),
            "a bare string prompt must be rejected, not coerced"
        );
        // an object (non-array, non-string) prompt is likewise rejected.
        assert!(
            extract_prompt_params(Some(&json!({"sessionId": "sess_x", "prompt": {"type": "text"}})))
                .is_err(),
            "a non-array prompt must be rejected"
        );
    }

    // --- transport auth gate (F1): no key allowed only on loopback ---

    #[test]
    fn acp_auth_gate_requires_key_off_loopback() {
        use super::acp_auth_ok_for_bind;
        // a non-empty key suffices on any bind
        assert!(acp_auth_ok_for_bind(Some("k"), "0.0.0.0:8080").is_ok());
        assert!(acp_auth_ok_for_bind(Some("k"), "127.0.0.1:8080").is_ok());
        // no key: loopback binds are allowed
        assert!(acp_auth_ok_for_bind(None, "127.0.0.1:8080").is_ok());
        assert!(acp_auth_ok_for_bind(None, "localhost:8080").is_ok());
        assert!(acp_auth_ok_for_bind(None, "[::1]:8080").is_ok());
        // no key: non-loopback binds are refused
        assert!(acp_auth_ok_for_bind(None, "0.0.0.0:8080").is_err());
        assert!(acp_auth_ok_for_bind(None, "192.168.1.10:8080").is_err());
        // an empty key is treated as no key
        assert!(acp_auth_ok_for_bind(Some(""), "0.0.0.0:8080").is_err());
        assert!(acp_auth_ok_for_bind(Some(""), "127.0.0.1:8080").is_ok());
    }

    #[test]
    fn subprotocol_token_extraction() {
        use super::subprotocol_token;
        use axum::http::HeaderMap;
        let mut h = HeaderMap::new();
        assert_eq!(subprotocol_token(&h), None); // no header
        // the browser offers "openab.bearer.<token>, acp.v1" → extract the token
        h.insert("sec-websocket-protocol", "openab.bearer.abc123, acp.v1".parse().unwrap());
        assert_eq!(subprotocol_token(&h), Some("abc123"));
        // only the real protocol, no bearer entry → None
        h.insert("sec-websocket-protocol", "acp.v1".parse().unwrap());
        assert_eq!(subprotocol_token(&h), None);
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
mod acp_requests {
    //! T1 — the agent→client REQUEST direction: server-initiated `send_request` (mints an
    //! id, awaits the correlated response) and inbound `route_client_response`.
    use super::{mcp_connect, mcp_message_request, route_client_response, send_request};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot};

    fn new_pending(
    ) -> Arc<tokio::sync::Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn route_client_response_resolves_pending() {
        let pending = new_pending();
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(5, tx);
        let consumed =
            route_client_response(&pending, &json!({"jsonrpc":"2.0","id":5,"result":{"ok":true}}))
                .await;
        assert!(consumed, "an id+result frame is a response we consume");
        assert_eq!(rx.await.unwrap()["result"]["ok"], json!(true));
        assert!(
            pending.lock().await.is_empty(),
            "the pending entry must be removed"
        );
    }

    #[tokio::test]
    async fn route_client_response_ignores_requests_and_notifications() {
        let pending = new_pending();
        // has `method` → a request, not a response
        assert!(!route_client_response(&pending, &json!({"jsonrpc":"2.0","id":1,"method":"foo"})).await);
        // notification-shaped, no result/error → not a response
        assert!(!route_client_response(&pending, &json!({"jsonrpc":"2.0","method":"bar"})).await);
        // id present but neither result nor error → not a response
        assert!(!route_client_response(&pending, &json!({"jsonrpc":"2.0","id":2})).await);
    }

    #[tokio::test]
    async fn route_client_response_unknown_id_consumes_without_panic() {
        let pending = new_pending();
        let consumed = route_client_response(
            &pending,
            &json!({"jsonrpc":"2.0","id":99,"error":{"code":-1,"message":"x"}}),
        )
        .await;
        assert!(consumed, "an unmatched response is still consumed (logged, no panic)");
    }

    #[tokio::test]
    async fn send_request_mints_incrementing_ids_and_returns_response() {
        let pending = new_pending();
        let next_id = AtomicU64::new(1);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

        // Driver: read the emitted request frame, assert its shape, feed a matching response.
        let pending2 = pending.clone();
        let driver = tokio::spawn(async move {
            let frame = out_rx.recv().await.unwrap();
            let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert_eq!(v["id"], json!(1));
            assert_eq!(v["method"], json!("mcp/message"));
            assert_eq!(v["params"]["connectionId"], json!("conn-1"));
            let id = v["id"].as_u64().unwrap();
            route_client_response(&pending2, &json!({"jsonrpc":"2.0","id":id,"result":{"pong":true}}))
                .await;
        });

        let resp = send_request(
            &out_tx,
            &pending,
            &next_id,
            "mcp/message",
            json!({"connectionId":"conn-1"}),
            5,
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["pong"], json!(true));
        driver.await.unwrap();
        assert_eq!(next_id.load(Ordering::Relaxed), 2, "the id counter advanced");
    }

    #[tokio::test]
    async fn mcp_tunnel_connect_and_message_roundtrip() {
        let pending = new_pending();
        let next_id = AtomicU64::new(1);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

        // Mock extension: answer mcp/connect with a connectionId, then turn an mcp/message
        // tools/list into an inner result, routing each reply by the frame's own outer id.
        let pending2 = pending.clone();
        let ext = tokio::spawn(async move {
            let f1: serde_json::Value = serde_json::from_str(&out_rx.recv().await.unwrap()).unwrap();
            assert_eq!(f1["method"], json!("mcp/connect"));
            assert_eq!(f1["params"]["acpId"], json!("srv-1"));
            route_client_response(
                &pending2,
                &json!({"jsonrpc":"2.0","id":f1["id"],"result":{"connectionId":"conn-9"}}),
            )
            .await;

            let f2: serde_json::Value = serde_json::from_str(&out_rx.recv().await.unwrap()).unwrap();
            assert_eq!(f2["method"], json!("mcp/message"));
            assert_eq!(f2["params"]["connectionId"], json!("conn-9"));
            assert_eq!(f2["params"]["method"], json!("tools/list"));
            route_client_response(
                &pending2,
                &json!({"jsonrpc":"2.0","id":f2["id"],"result":{"tools":[{"name":"browser.click"}]}}),
            )
            .await;
        });

        let conn = mcp_connect(&out_tx, &pending, &next_id, "srv-1", 5)
            .await
            .unwrap();
        assert_eq!(conn, "conn-9");
        let result = mcp_message_request(&out_tx, &pending, &next_id, &conn, "tools/list", None, 5)
            .await
            .unwrap();
        assert_eq!(result["tools"][0]["name"], json!("browser.click"));
        ext.await.unwrap();
    }
}

#[cfg(test)]
mod acp_handlers {
    use super::{
        handle_initialize, handle_session_new, handle_session_resume, parse_acp_mcp_servers,
        AcpMcpServer, AcpSession, JsonRpcRequest,
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
        let v = serde_json::to_value(handle_session_new(&sessions, json!(2), vec![]).await).unwrap();
        let sid = v["result"]["sessionId"].as_str().unwrap();
        assert!(sid.starts_with("sess_"), "sessionId must be sess_<uuid>: {sid}");
        assert!(sessions.lock().await.contains_key(sid), "session must be stored");
    }

    #[test]
    fn parse_acp_mcp_servers_keeps_only_acp_entries() {
        let params = json!({
            "cwd": "/w",
            "mcpServers": [
                {"type": "acp", "id": "srv-1", "name": "browser"},
                {"type": "http", "url": "http://x"},
                {"type": "acp", "id": "srv-2", "name": "other"}
            ]
        });
        assert_eq!(
            parse_acp_mcp_servers(Some(&params)),
            vec![
                AcpMcpServer { id: "srv-1".into(), name: "browser".into() },
                AcpMcpServer { id: "srv-2".into(), name: "other".into() },
            ]
        );
        // no mcpServers -> empty
        assert!(parse_acp_mcp_servers(Some(&json!({"cwd": "/w"}))).is_empty());
        assert!(parse_acp_mcp_servers(None).is_empty());
    }

    #[tokio::test]
    async fn session_new_records_declared_acp_servers() {
        let sessions = new_sessions();
        let servers = vec![AcpMcpServer { id: "srv-1".into(), name: "browser".into() }];
        let v = serde_json::to_value(
            handle_session_new(&sessions, json!(2), servers.clone()).await,
        )
        .unwrap();
        let sid = v["result"]["sessionId"].as_str().unwrap().to_string();
        let guard = sessions.lock().await;
        assert_eq!(guard.get(&sid).unwrap().acp_mcp_servers, servers);
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

// ---------------------------------------------------------------------------
// Group-review fixes (M1 resume cap / M2 stale-reply fence / subprotocol charset).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod acp_review_fixes {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    fn sessions_map() -> Arc<tokio::sync::Mutex<HashMap<String, AcpSession>>> {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    // M1 — session/resume enforces the same per-connection cap as session/new, so a
    // client cannot grow the map without bound by resuming arbitrary `sess_<uuid>`.
    #[tokio::test]
    async fn resume_enforces_session_cap() {
        let sessions = sessions_map();
        let mut ids = Vec::new();
        for _ in 0..MAX_SESSIONS_PER_CONNECTION {
            let sid = format!("sess_{}", Uuid::new_v4());
            let p = json!({ "sessionId": sid });
            let v = serde_json::to_value(handle_session_resume(&sessions, json!(1), Some(&p)).await)
                .unwrap();
            assert_eq!(v["result"], json!({}), "resume under cap should succeed");
            ids.push(sid);
        }
        assert_eq!(sessions.lock().await.len(), MAX_SESSIONS_PER_CONNECTION);
        // A new distinct session over the cap is refused with ACP_OVERLOADED.
        let over = json!({ "sessionId": format!("sess_{}", Uuid::new_v4()) });
        let v = serde_json::to_value(handle_session_resume(&sessions, json!(2), Some(&over)).await)
            .unwrap();
        assert_eq!(v["error"]["code"], json!(ACP_OVERLOADED), "over-cap resume must be refused");
        // Re-resuming an already-present session is exempt (idempotent).
        let existing = json!({ "sessionId": ids[0] });
        let v =
            serde_json::to_value(handle_session_resume(&sessions, json!(3), Some(&existing)).await)
                .unwrap();
        assert_eq!(v["result"], json!({}), "re-resume of existing session must bypass the cap");
    }

    fn reply(channel_id: &str, reply_to: &str, text: &str, command: Option<&str>) -> GatewayReply {
        GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: reply_to.into(),
            platform: "acp".into(),
            channel: crate::schema::ReplyChannel { id: channel_id.into(), thread_id: None },
            content: crate::schema::Content {
                content_type: "text".into(),
                text: text.into(),
                attachments: Vec::new(),
            },
            command: command.map(|c| c.into()),
            request_id: None,
            quote_message_id: None,
        }
    }

    // M2 — a late reply carrying a superseded turn's event id is dropped, not delivered
    // into the current turn's stream; a reply matching the active turn is delivered.
    #[tokio::test]
    async fn handle_reply_fences_stale_turn() {
        let registry = new_reply_registry();
        let (tx, mut rx) = mpsc::unbounded_channel::<ReplyChunk>();
        registry
            .lock()
            .unwrap()
            .insert("acp_chan".into(), ReplySink { turn_id: "evt_current".into(), tx });

        // Stale reply (previous turn's event id) → dropped.
        handle_reply(&reply("acp_chan", "evt_stale", "leaked", Some("edit_message")), &registry)
            .await;
        assert!(rx.try_recv().is_err(), "stale reply must not reach the active turn");

        // Matching reply → delivered.
        handle_reply(&reply("acp_chan", "evt_current", "hello", Some("edit_message")), &registry)
            .await;
        match rx.try_recv() {
            Ok(ReplyChunk::Text(t)) => assert_eq!(t, "hello"),
            _ => panic!("expected the matching reply to be delivered"),
        }
    }

    // R17-F1 — keyless-mode browser `Origin` gating. A WS handshake bypasses the browser
    // same-origin policy, so on a keyless loopback bind an un-allowlisted browser origin
    // must be refused (ws_upgrade turns `false` into a 403). A non-browser client sends no
    // `Origin` and is always admitted; the keyed path never reaches this check (it lives in
    // the `else` of the bearer branch), so a keyed bind is unaffected by the allowlist.
    #[test]
    fn acp_origin_ok_keyless_gating() {
        let allow = vec!["https://app.example".to_string(), "http://localhost:5173".to_string()];
        // Absent Origin (non-browser client) → accept, regardless of allowlist.
        assert!(acp_origin_ok(None, &allow), "no Origin (non-browser) must be admitted");
        assert!(acp_origin_ok(None, &[]), "no Origin must be admitted even with empty allowlist");
        // Allowlisted browser Origin → accept (exact match, both entries).
        assert!(acp_origin_ok(Some("https://app.example"), &allow));
        assert!(acp_origin_ok(Some("http://localhost:5173"), &allow));
        // Disallowed browser Origin → reject (→ 403 at the handler).
        assert!(!acp_origin_ok(Some("https://evil.example"), &allow));
        // Default empty allowlist blocks every browser-set Origin.
        assert!(!acp_origin_ok(Some("https://app.example"), &[]));
        // Match is exact — no scheme/host/port fuzzing, no trailing-slash leniency.
        assert!(!acp_origin_ok(Some("https://app.example/"), &allow));
        assert!(!acp_origin_ok(Some("http://app.example"), &allow));
    }

    // R17-F2 — the legacy `?token=` query fallback is gone. The bearer is extracted ONLY
    // from `Authorization: Bearer` or the `Sec-WebSocket-Protocol` subprotocol; `ws_upgrade`
    // no longer reads the query string. A request whose only credential would have been
    // `?token=<key>` now carries no header-borne token, so extraction yields None → keyed
    // mode rejects it 401 (the query is never consulted).
    #[test]
    fn ws_bearer_token_ignores_query_only_request() {
        use axum::http::HeaderMap;
        // No Authorization, no subprotocol → what used to be a `?token=` request now has no
        // extractable bearer (None → 401 in keyed mode).
        assert_eq!(ws_bearer_token(&HeaderMap::new()), None);
        // Authorization: Bearer still carries the key.
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer sekret".parse().unwrap());
        assert_eq!(ws_bearer_token(&h), Some("sekret"));
        // The subprotocol path still carries the key.
        let mut h = HeaderMap::new();
        h.insert("sec-websocket-protocol", "openab.bearer.sekret, acp.v1".parse().unwrap());
        assert_eq!(ws_bearer_token(&h), Some("sekret"));
    }

    // subprotocol charset (n1) — base64 `/` and `=` are not RFC 6455 token chars; the
    // recommended `[A-Za-z0-9._~+-]` set (plus other tchars) is.
    #[test]
    fn ws_subprotocol_token_charset() {
        for &b in b"AZaz09._~+-!#$%&'*^`|" {
            assert!(is_ws_subprotocol_token_char(b), "{} should be token-safe", b as char);
        }
        for &b in b"=/,; @\"" {
            assert!(!is_ws_subprotocol_token_char(b), "{} should be rejected", b as char);
        }
    }

    // R16-F1 — the read loop now reserves the prompt's cancel state SYNCHRONOUSLY (busy + a
    // cancel Notify installed under the session lock) before spawning the handler. So a
    // `session/cancel` arriving before the handler reaches its stream `select!` still cancels
    // the turn: `tokio::Notify` stores one permit, so a pre-fired cancel is consumed by
    // `cancel.notified()` (stopReason "cancelled") rather than lost. Before the fix the cancel
    // installed inside the spawned task, so an immediate cancel read `s.cancel == None`.
    #[tokio::test]
    async fn prompt_cancel_race_before_first_update_cancels() {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let mut st = crate::AppState::test_default(event_tx);
        st.acp_reply_registry = Some(new_reply_registry());
        let state = Arc::new(st);

        let sessions = sessions_map();
        let sid = format!("sess_{}", Uuid::new_v4());
        let cancel = Arc::new(tokio::sync::Notify::new());
        sessions.lock().await.insert(
            sid.clone(),
            AcpSession {
                channel_id: format!("acp_{}", Uuid::new_v4()),
                busy: true,
                cancel: Some(cancel.clone()),
            },
        );
        // Cancel arrives before the handler's stream loop (reserved-then-immediate-cancel).
        cancel.notify_one();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let params = json!({"sessionId": sid, "prompt": [{"type": "text", "text": "hi"}]});
        handle_session_prompt(&state, &sessions, json!(7), Some(&params), &out_tx, sid.clone(), cancel)
            .await;

        // The final response (matching our request id) must carry stopReason "cancelled".
        let mut final_resp = None;
        while let Ok(s) = out_rx.try_recv() {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            if v.get("id") == Some(&json!(7)) {
                final_resp = Some(v);
            }
        }
        let resp = final_resp.expect("prompt must produce a final response");
        assert_eq!(
            resp["result"]["stopReason"],
            json!("cancelled"),
            "an immediate cancel must cancel the turn, not be dropped"
        );
        // And the reservation is released.
        let g = sessions.lock().await;
        let s = g.get(&sid).unwrap();
        assert!(!s.busy && s.cancel.is_none(), "cancel must release busy + cancel handle");
    }

    // R16-F2 — session/resume on a session with a prompt in flight is rejected (busy), so the
    // active turn's cancel handle + state are NOT clobbered by resume's unconditional rewrite.
    #[tokio::test]
    async fn resume_while_busy_is_rejected_and_preserves_state() {
        let sessions = sessions_map();
        let sid = format!("sess_{}", Uuid::new_v4());
        let cancel = Arc::new(tokio::sync::Notify::new());
        sessions.lock().await.insert(
            sid.clone(),
            AcpSession {
                channel_id: format!("acp_{}", Uuid::new_v4()),
                busy: true,
                cancel: Some(cancel.clone()),
            },
        );

        let params = json!({"sessionId": sid, "cwd": "/w", "mcpServers": []});
        let v =
            serde_json::to_value(handle_session_resume(&sessions, json!(9), Some(&params)).await)
                .unwrap();
        assert_eq!(v["error"]["code"], json!(-32001), "resume while busy must be rejected");

        // The in-flight turn's state survives untouched.
        let g = sessions.lock().await;
        let s = g.get(&sid).unwrap();
        assert!(s.busy, "busy must remain set after a rejected resume");
        assert!(s.cancel.is_some(), "the active prompt's cancel handle must survive resume");
    }

    // R16-F3(A) — Phase-1 send-once: the ACP path streams the whole reply as a SINGLE terminal
    // agent_message_chunk (backend streaming=false), which anchors the ADR/PR doc claim. A final
    // reply (`send_message`) delivers one Text + Done, so exactly one chunk reaches the client.
    #[tokio::test]
    async fn phase1_emits_single_terminal_agent_message_chunk() {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<String>(16);
        let registry = new_reply_registry();
        let mut st = crate::AppState::test_default(event_tx);
        st.acp_reply_registry = Some(registry.clone());
        let state = Arc::new(st);

        let sessions = sessions_map();
        let sid = format!("sess_{}", Uuid::new_v4());
        let channel_id = format!("acp_{}", Uuid::new_v4());
        let cancel = Arc::new(tokio::sync::Notify::new());
        sessions.lock().await.insert(
            sid.clone(),
            AcpSession { channel_id: channel_id.clone(), busy: true, cancel: Some(cancel.clone()) },
        );

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let st2 = state.clone();
        let sessions2 = sessions.clone();
        let sid2 = sid.clone();
        let handle = tokio::spawn(async move {
            let params = json!({"sessionId": sid2, "prompt": [{"type": "text", "text": "hi"}]});
            handle_session_prompt(&st2, &sessions2, json!(11), Some(&params), &out_tx, sid2.clone(), cancel)
                .await;
        });

        // Wait for the handler to register its reply sink, then feed one final reply.
        let mut turn_id = None;
        for _ in 0..10_000 {
            if let Some(t) = registry.lock().unwrap().get(&channel_id).map(|s| s.turn_id.clone()) {
                turn_id = Some(t);
                break;
            }
            tokio::task::yield_now().await;
        }
        let turn_id = turn_id.expect("handler must register a reply sink");
        handle_reply(&reply(&channel_id, &turn_id, "hello world", Some("send_message")), &registry).await;
        handle.await.unwrap();

        let mut chunks = Vec::new();
        let mut final_stop = None;
        while let Ok(s) = out_rx.try_recv() {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            if v["method"] == json!("session/update")
                && v["params"]["update"]["sessionUpdate"] == json!("agent_message_chunk")
            {
                chunks.push(v["params"]["update"]["content"]["text"].as_str().unwrap_or("").to_string());
            }
            if v.get("id") == Some(&json!(11)) {
                final_stop = v["result"]["stopReason"].as_str().map(str::to_string);
            }
        }
        assert_eq!(chunks.len(), 1, "Phase-1 must stream exactly one terminal chunk, got {chunks:?}");
        assert_eq!(chunks[0], "hello world");
        assert_eq!(final_stop.as_deref(), Some("end_turn"), "a completed turn ends end_turn");
    }

    // R17-F3c — a request-shaped `session/cancel` (id present) must NOT be acknowledged with
    // an empty success frame. ACP defines cancel as notification-only, so a request form is a
    // protocol violation → -32600 invalid request, and the cancel signal is not fired.
    #[tokio::test]
    async fn cancel_as_request_is_rejected_not_empty_success() {
        let sessions = sessions_map();
        let params = json!({"sessionId": format!("sess_{}", Uuid::new_v4())});
        let resp = handle_session_cancel(&sessions, json!(42), Some(&params), false)
            .await
            .expect("a request-shaped cancel must produce a response, not silence");
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["error"]["code"], json!(-32600), "request-shaped cancel must be -32600");
        assert_eq!(v["id"], json!(42));
        assert!(
            v.get("result").is_none(),
            "must NOT carry an empty-success result: {v}"
        );
    }

    // R17-F3c — the notification form (no id) still fires the session's cancel signal and
    // returns no response frame, unchanged.
    #[tokio::test]
    async fn cancel_as_notification_fires_signal_and_returns_no_response() {
        let sessions = sessions_map();
        let sid = format!("sess_{}", Uuid::new_v4());
        let cancel = Arc::new(tokio::sync::Notify::new());
        sessions.lock().await.insert(
            sid.clone(),
            AcpSession {
                channel_id: format!("acp_{}", Uuid::new_v4()),
                busy: true,
                cancel: Some(cancel.clone()),
            },
        );
        let params = json!({"sessionId": sid});
        let resp = handle_session_cancel(&sessions, Value::Null, Some(&params), true).await;
        assert!(resp.is_none(), "a notification cancel must produce no response frame");
        // notify_one stored a permit, so notified() resolves immediately — the signal fired.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), cancel.notified())
                .await
                .is_ok(),
            "notification cancel must fire the session's cancel signal"
        );
    }
}

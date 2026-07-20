//! Core-hosted MCP proxy server for MCP-over-ACP browser control (feature `acp-mcp`).
//!
//! Per ADR §7 (D3), OpenAB **core** hosts an in-process Streamable-HTTP MCP server on
//! loopback that the colocated agent CLI connects to as a normal MCP client. The server is a
//! proxy: its tool list + tool execution are backed by the remote browser extension over the
//! `/acp` MCP-over-ACP tunnel (wired in T5.3). Per D4 the browser tool set is
//! **static-advertised** regardless of whether an extension is currently attached — a call
//! while disconnected returns a "browser not connected" error rather than hiding the tools.
//!
//! This module currently provides the static tool set; the `ServerHandler` + loopback
//! listener (`spawn_mcp_server`) and the tunnel wiring land in the following T5 sub-ticks.

use rmcp::model::{
    object, CallToolRequestParams, CallToolResult, ErrorData as McpError, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::ServerHandler;
use axum::response::IntoResponse;
use serde_json::{json, Value};
use std::sync::Arc;

/// Core-side interface to the browser MCP-over-ACP tunnel (D6-a'). Implemented by the ROOT
/// (which bridges to the gateway's per-connection tunnel registry) and consumed by the MCP
/// proxy here. Keeping the trait in core with the impl in root preserves the core/gateway
/// sibling independence, matching the existing `ChatAdapter` pattern.
#[async_trait::async_trait]
pub trait BrowserTunnel: Send + Sync {
    /// Forward an inner MCP request (e.g. `tools/call`) to the browser session identified by
    /// `channel_id` and return the inner MCP result payload. Err if no browser is currently
    /// attached to that session.
    async fn call(
        &self,
        channel_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String>;
}

/// The fixed set of browser tools OpenAB advertises over MCP (D4 static-advertise). DOM-
/// semantic actions the extension executes in the user's active tab; model-agnostic.
pub(crate) fn browser_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "browser.click",
            "Click the element matching a CSS selector in the active browser tab.",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "CSS selector" } },
                "required": ["selector"]
            })),
        ),
        Tool::new(
            "browser.read_dom",
            "Read a snapshot of the active tab's DOM (optionally scoped to a selector).",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "optional CSS selector to scope the snapshot" } }
            })),
        ),
        Tool::new(
            "browser.navigate",
            "Navigate the active browser tab to a URL.",
            object(json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "absolute URL" } },
                "required": ["url"]
            })),
        ),
        Tool::new(
            "browser.type",
            "Type text into the element matching a CSS selector in the active tab.",
            object(json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector" },
                    "text": { "type": "string", "description": "text to type" }
                },
                "required": ["selector", "text"]
            })),
        ),
        Tool::new(
            "browser.screenshot",
            "Capture a screenshot of the active browser tab.",
            object(json!({ "type": "object", "properties": {} })),
        ),
    ]
}

/// The core-hosted MCP server the colocated agent connects to (D3). A proxy: it advertises
/// the browser tools and (once T5.3 wires the tunnel) forwards `tools/call` to the extension
/// over MCP-over-ACP. Until then it static-advertises (D4) and returns "browser not
/// connected" on call.
#[derive(Clone)]
pub struct ProxyHandler {
    /// The browser session this server instance serves (D5-a: one MCP server per session).
    channel_id: String,
    /// Bridge to that session's browser tunnel; `None` when no browser is attached (or the
    /// process has no tunnel wiring). A call while `None` reports "browser not connected" (D4).
    tunnel: Option<Arc<dyn BrowserTunnel>>,
}

impl ProxyHandler {
    pub fn new(channel_id: String, tunnel: Option<Arc<dyn BrowserTunnel>>) -> Self {
        Self { channel_id, tunnel }
    }

    /// Forward a tool call to the browser over the tunnel (as an MCP `tools/call`), or report
    /// not-connected (D4) when no browser is attached.
    async fn forward_tool_call(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResult, McpError> {
        let Some(tunnel) = &self.tunnel else {
            return Err(McpError::internal_error(
                "browser not connected: open the OpenAB side panel in your browser",
                None,
            ));
        };
        let params = json!({ "name": name, "arguments": arguments });
        let result = tunnel
            .call(&self.channel_id, "tools/call", Some(params))
            .await
            .map_err(|e| McpError::internal_error(e, None))?;
        serde_json::from_value(result)
            .map_err(|e| McpError::internal_error(format!("malformed tool result: {e}"), None))
    }
}

impl ServerHandler for ProxyHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "OpenAB browser-control proxy: DOM-semantic tools executed in the user's browser \
             via MCP-over-ACP.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // D4 static-advertise: expose the browser tools regardless of extension state.
        Ok(ListToolsResult {
            tools: browser_tools(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.forward_tool_call(request.name.as_ref(), request.arguments)
            .await
    }
}

/// Loopback bearer gate for the MCP server (D3): even bound to 127.0.0.1, require the token
/// the agent's MCP config carries, so another local process on the host can't reach the
/// browser tools. Returns 401 when the `Authorization: Bearer <token>` header is absent or
/// wrong.
async fn require_bearer(
    axum::extract::State(expected): axum::extract::State<Arc<str>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let authed = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        // Constant-time compare so a wrong token can't be probed byte-by-byte via response
        // timing (mirrors the gateway's feishu/wecom signature checks).
        .is_some_and(|t| {
            use subtle::ConstantTimeEq;
            t.as_bytes().ct_eq(expected.as_bytes()).into()
        });
    if authed {
        next.run(req).await
    } else {
        axum::http::StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Start the in-process Streamable-HTTP MCP proxy server on an OS-assigned **loopback** port
/// (D3), gated by `bearer`. Returns the bound address; the caller hands `addr.port()` + the
/// same `bearer` to the colocated agent's native MCP config (T5.2). Shuts down when `ct` is
/// cancelled.
pub async fn spawn_mcp_server(
    channel_id: String,
    tunnel: Option<Arc<dyn BrowserTunnel>>,
    bearer: String,
    ct: tokio_util::sync::CancellationToken,
) -> std::io::Result<std::net::SocketAddr> {
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(ct.child_token());
    let service: StreamableHttpService<ProxyHandler, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(ProxyHandler::new(channel_id.clone(), tunnel.clone())),
            Default::default(),
            config,
        );
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(bearer),
            require_bearer,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await;
    });
    Ok(addr)
}

/// Start a per-session MCP proxy server (D5-a) and register it in the agent's native MCP
/// config so the colocated agent connects to it (D2). Mints a fresh bearer, starts the
/// loopback server, and writes/merges `<workdir>/.cursor/mcp.json` with the `openab-browser`
/// HTTP entry (Cursor's config; other agents get their own writer later). Returns the bound
/// address + the `CancellationToken` the caller cancels to stop the server on session evict.
pub async fn start_session_server(
    channel_id: &str,
    workdir: &str,
    tunnel: Option<Arc<dyn BrowserTunnel>>,
) -> std::io::Result<(std::net::SocketAddr, tokio_util::sync::CancellationToken)> {
    let bearer = uuid::Uuid::new_v4().to_string();
    let ct = tokio_util::sync::CancellationToken::new();
    let addr = spawn_mcp_server(channel_id.to_string(), tunnel, bearer.clone(), ct.clone()).await?;

    let our_url = format!("http://{addr}/mcp");
    let entry = json!({
        "url": our_url.clone(),
        "headers": { "Authorization": format!("Bearer {bearer}") }
    });

    // Merge the openab-browser entry into each colocated ACP CLI's native MCP config (don't
    // clobber servers the user/agent already configured). Cursor reads <workdir>/.cursor/mcp.json;
    // kiro-cli reads <workdir>/.kiro/settings/mcp.json. We write both — each CLI ignores the
    // other's file — so the browser server reaches whichever agent is colocated.
    let cfg_paths = [
        std::path::Path::new(workdir).join(".cursor").join("mcp.json"),
        std::path::Path::new(workdir)
            .join(".kiro")
            .join("settings")
            .join("mcp.json"),
    ];
    for cfg_path in &cfg_paths {
        if let Some(dir) = cfg_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut cfg: Value = match tokio::fs::read(cfg_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})),
            Err(_) => json!({}),
        };
        if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
            cfg["mcpServers"] = json!({});
        }
        cfg["mcpServers"]["openab-browser"] = entry.clone();
        // 0600: the file carries a live bearer token — default umask would leave it world-readable.
        write_private(cfg_path, &serde_json::to_vec_pretty(&cfg)?).await?;
    }

    // On session evict/drop the caller cancels `ct`; strip our now-dead `openab-browser` entry
    // (with its live bearer) from each config so a stale credential doesn't linger. Only remove it
    // if it still points at OUR addr — a concurrent/reconnected session may have already replaced
    // it, and we must not clobber that live entry (the mcp.json paths are shared across acp: sessions).
    let cleanup_paths = cfg_paths.to_vec();
    let cleanup_url = our_url;
    let cleanup_ct = ct.clone();
    tokio::spawn(async move {
        cleanup_ct.cancelled().await;
        for cleanup_path in &cleanup_paths {
            let Ok(bytes) = tokio::fs::read(cleanup_path).await else {
                continue;
            };
            let Ok(mut cfg) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let still_ours = cfg
                .pointer("/mcpServers/openab-browser/url")
                .and_then(Value::as_str)
                == Some(cleanup_url.as_str());
            if !still_ours {
                continue;
            }
            if let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) {
                servers.remove("openab-browser");
            }
            if let Ok(out) = serde_json::to_vec_pretty(&cfg) {
                let _ = write_private(cleanup_path, &out).await;
            }
        }
    });

    Ok((addr, ct))
}

/// Write `bytes` to `path`, then tighten it to owner-only (0600). The file holds a live bearer
/// token for the loopback MCP server, so it must not be group/world readable.
async fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    tokio::fs::write(path, bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

// ---- Option C: per-pod stdio-bridge socket server -------------------------------------------
// A single unix socket per pod multiplexes ALL sessions. The `openab browser-bridge` shim
// (spawned per agent session by the CLI's MCP client) connects and forwards inner MCP requests
// tagged with its own `channel_id` (from the OPENAB_BROWSER_CHANNEL env it inherits); core routes
// `tools/call` to that session's BrowserTunnel. This is the stable, variant-agnostic replacement
// for the per-session HTTP proxy (Option C). Wire = newline-delimited JSON, one frame per line:
//   bridge → core : {"channel_id": "...", "request": <inner MCP JSON-RPC request>}
//   core → bridge : <inner MCP JSON-RPC response>   (omitted for notifications)

const BROWSER_MCP_PROTOCOL_VERSION: &str = "2025-06-18";

fn mcp_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn mcp_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Dispatch one inner MCP request for `channel_id`, backing `tools/call` with the shared browser
/// tunnel. Returns the MCP response, or `None` for a JSON-RPC notification (no reply). Same tool
/// set + not-connected semantics as the HTTP `ProxyHandler` (single source of truth).
pub(crate) async fn dispatch_browser_mcp(
    channel_id: &str,
    request: &Value,
    tunnel: &Option<Arc<dyn BrowserTunnel>>,
) -> Option<Value> {
    // A JSON-RPC notification has no `id` → no response.
    let id = request.get("id").cloned()?;
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let resp = match method {
        "initialize" => mcp_result(
            id,
            json!({
                "protocolVersion": BROWSER_MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "openab-browser", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "tools/list" => {
            let tools = serde_json::to_value(browser_tools()).unwrap_or_else(|_| json!([]));
            mcp_result(id, json!({ "tools": tools }))
        }
        "tools/call" => match tunnel {
            Some(t) => match t
                .call(channel_id, "tools/call", request.get("params").cloned())
                .await
            {
                Ok(v) => mcp_result(id, v),
                Err(e) => mcp_error(id, -32603, &e),
            },
            None => mcp_error(
                id,
                -32603,
                "browser not connected: open the OpenAB side panel in your browser",
            ),
        },
        other => mcp_error(id, -32601, &format!("method not found: {other}")),
    };
    Some(resp)
}

/// Serve the per-pod browser-bridge socket at `path`, routing each connection's framed requests
/// via [`dispatch_browser_mcp`]. Binds a fresh 0600 unix socket (same-uid only), spawns the accept
/// loop, and runs until `ct` is cancelled. Idempotent on a stale socket file from a prior run.
pub async fn serve_browser_socket(
    path: std::path::PathBuf,
    tunnel: Option<Arc<dyn BrowserTunnel>>,
    ct: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    let _ = tokio::fs::remove_file(&path).await; // clear a stale socket from a prior run
    let listener = tokio::net::UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            tokio::spawn(handle_browser_conn(stream, tunnel.clone()));
                        }
                        Err(_) => continue,
                    }
                }
            }
        }
        let _ = tokio::fs::remove_file(&path).await;
    });
    Ok(())
}

async fn handle_browser_conn(
    stream: tokio::net::UnixStream,
    tunnel: Option<Arc<dyn BrowserTunnel>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue; // skip a malformed frame rather than drop the connection
        };
        let channel_id = frame.get("channel_id").and_then(Value::as_str).unwrap_or("");
        let Some(request) = frame.get("request") else {
            continue;
        };
        if let Some(resp) = dispatch_browser_mcp(channel_id, request, &tunnel).await {
            let Ok(mut buf) = serde_json::to_vec(&resp) else {
                continue;
            };
            buf.push(b'\n');
            if write_half.write_all(&buf).await.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        browser_tools, dispatch_browser_mcp, serve_browser_socket, spawn_mcp_server,
        start_session_server, BrowserTunnel, ProxyHandler,
    };

    struct MockTunnel;
    #[async_trait::async_trait]
    impl BrowserTunnel for MockTunnel {
        async fn call(
            &self,
            channel_id: &str,
            method: &str,
            _params: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            assert_eq!(channel_id, "acp_x");
            assert_eq!(method, "tools/call");
            Ok(serde_json::json!({"content": [{"type": "text", "text": "clicked"}]}))
        }
    }

    #[tokio::test]
    async fn call_tool_forwards_to_the_tunnel() {
        let h = ProxyHandler::new("acp_x".into(), Some(std::sync::Arc::new(MockTunnel)));
        let result = h.forward_tool_call("browser.click", None).await.unwrap();
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["content"][0]["text"], serde_json::json!("clicked"));
    }

    #[tokio::test]
    async fn call_tool_reports_not_connected_without_a_tunnel() {
        let h = ProxyHandler::new("acp_x".into(), None);
        assert!(
            h.forward_tool_call("browser.click", None).await.is_err(),
            "a call with no attached browser must error (D4)"
        );
    }

    // --- Option C: browser-bridge socket dispatch ---
    struct RecordTunnel {
        result: serde_json::Value,
    }
    #[async_trait::async_trait]
    impl BrowserTunnel for RecordTunnel {
        async fn call(
            &self,
            channel_id: &str,
            method: &str,
            _params: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            assert_eq!(method, "tools/call");
            assert_eq!(channel_id, "acp_win1");
            Ok(self.result.clone())
        }
    }
    struct ErrTunnel;
    #[async_trait::async_trait]
    impl BrowserTunnel for ErrTunnel {
        async fn call(
            &self,
            _c: &str,
            _m: &str,
            _p: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            Err("no browser attached".into())
        }
    }
    fn req(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }
    fn arc_tunnel<T: BrowserTunnel + 'static>(t: T) -> Option<std::sync::Arc<dyn BrowserTunnel>> {
        Some(std::sync::Arc::new(t))
    }

    #[tokio::test]
    async fn dispatch_initialize_advertises_tools() {
        let r = dispatch_browser_mcp("acp_x", &req(1, "initialize", serde_json::json!({})), &None)
            .await
            .unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["capabilities"]["tools"], serde_json::json!({}));
        assert_eq!(r["result"]["serverInfo"]["name"], "openab-browser");
    }

    #[tokio::test]
    async fn dispatch_tools_list_returns_five_tools() {
        let r = dispatch_browser_mcp("acp_x", &req(2, "tools/list", serde_json::json!({})), &None)
            .await
            .unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn dispatch_tools_call_routes_to_the_channel_tunnel() {
        let tunnel = arc_tunnel(RecordTunnel {
            result: serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] }),
        });
        let r = dispatch_browser_mcp(
            "acp_win1",
            &req(3, "tools/call", serde_json::json!({ "name": "browser.read_dom", "arguments": {} })),
            &tunnel,
        )
        .await
        .unwrap();
        assert_eq!(r["result"]["content"][0]["text"], "ok");
    }

    #[tokio::test]
    async fn dispatch_tools_call_without_tunnel_is_not_connected() {
        let r = dispatch_browser_mcp(
            "acp_x",
            &req(4, "tools/call", serde_json::json!({ "name": "browser.click" })),
            &None,
        )
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32603);
        assert!(r["error"]["message"].as_str().unwrap().contains("not connected"));
    }

    #[tokio::test]
    async fn dispatch_tools_call_surfaces_tunnel_error() {
        let tunnel = arc_tunnel(ErrTunnel);
        let r = dispatch_browser_mcp(
            "acp_x",
            &req(5, "tools/call", serde_json::json!({ "name": "browser.click" })),
            &tunnel,
        )
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32603);
        assert_eq!(r["error"]["message"], "no browser attached");
    }

    #[tokio::test]
    async fn dispatch_notification_gets_no_response() {
        let notif = serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(dispatch_browser_mcp("acp_x", &notif, &None).await.is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_is_method_not_found() {
        let r = dispatch_browser_mcp("acp_x", &req(6, "bogus/thing", serde_json::json!({})), &None)
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn browser_socket_round_trip() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("browser.sock");
        let tunnel = arc_tunnel(RecordTunnel {
            result: serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] }),
        });
        let ct = tokio_util::sync::CancellationToken::new();
        serve_browser_socket(sock.clone(), tunnel, ct.clone())
            .await
            .unwrap();
        let stream = loop {
            match tokio::net::UnixStream::connect(&sock).await {
                Ok(s) => break s,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        let (rd, mut wr) = stream.into_split();
        let frame = serde_json::json!({
            "channel_id": "acp_win1",
            "request": req(9, "tools/call", serde_json::json!({ "name": "browser.read_dom", "arguments": {} }))
        });
        let mut line = serde_json::to_vec(&frame).unwrap();
        line.push(b'\n');
        wr.write_all(&line).await.unwrap();
        let mut resp = String::new();
        BufReader::new(rd).read_line(&mut resp).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], 9);
        assert_eq!(v["result"]["content"][0]["text"], "ok");
        ct.cancel();
    }

    #[tokio::test]
    async fn start_session_server_writes_cursor_config() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, ct) = start_session_server("acp_x", dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
        assert!(addr.ip().is_loopback());

        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(".cursor/mcp.json")).unwrap())
                .unwrap();
        let entry = &cfg["mcpServers"]["openab-browser"];
        assert_eq!(entry["url"], serde_json::json!(format!("http://{addr}/mcp")));
        assert!(entry["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .starts_with("Bearer "));
        // The file holds a live bearer — it must be owner-only (0600), not umask-default 0644.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(".cursor/mcp.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "mcp.json (live bearer) must be 0600");
        }
        ct.cancel();
    }

    #[tokio::test]
    async fn start_session_server_merges_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(
            cursor.join("mcp.json"),
            r#"{"mcpServers":{"other":{"url":"http://x"}}}"#,
        )
        .unwrap();
        let (_addr, ct) = start_session_server("acp_x", dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        assert!(
            cfg["mcpServers"]["other"].is_object(),
            "existing server must be preserved"
        );
        assert!(
            cfg["mcpServers"]["openab-browser"].is_object(),
            "openab-browser must be added"
        );
        ct.cancel();
    }

    #[test]
    fn browser_tools_advertises_the_fixed_set() {
        let tools = browser_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "browser.click",
                "browser.read_dom",
                "browser.navigate",
                "browser.type",
                "browser.screenshot"
            ]
        );
    }

    #[test]
    fn every_browser_tool_has_an_object_input_schema() {
        for t in browser_tools() {
            assert_eq!(
                t.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool {} must have an object input schema",
                t.name
            );
            assert!(t.description.is_some(), "tool {} needs a description", t.name);
        }
    }

    const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;

    #[tokio::test]
    async fn mcp_server_binds_loopback_and_initializes_with_bearer() {
        let ct = tokio_util::sync::CancellationToken::new();
        let addr = spawn_mcp_server("acp_test".into(), None, "secret-token".to_string(), ct.clone())
            .await
            .unwrap();
        assert!(addr.ip().is_loopback(), "MCP server must bind loopback only");

        let url = format!("http://{addr}/mcp");
        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", "Bearer secret-token")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(INIT_BODY)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["jsonrpc"], "2.0");
        assert!(body["result"].is_object(), "initialize must return a result");
        assert!(
            body["result"]["capabilities"]["tools"].is_object(),
            "server must advertise the tools capability"
        );
        ct.cancel();
    }

    #[tokio::test]
    async fn mcp_server_rejects_missing_or_wrong_bearer() {
        let ct = tokio_util::sync::CancellationToken::new();
        let addr = spawn_mcp_server("acp_test".into(), None, "secret-token".to_string(), ct.clone())
            .await
            .unwrap();
        let url = format!("http://{addr}/mcp");
        let client = reqwest::Client::new();

        // no Authorization header -> 401
        let no_auth = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(INIT_BODY)
            .send()
            .await
            .unwrap();
        assert_eq!(no_auth.status(), 401);

        // wrong token -> 401
        let wrong = client
            .post(&url)
            .header("Authorization", "Bearer nope")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(INIT_BODY)
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401);
        ct.cancel();
    }
}

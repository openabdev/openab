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

    // Merge the openab-browser entry into <workdir>/.cursor/mcp.json (don't clobber any
    // servers the user/agent already configured).
    let cursor_dir = std::path::Path::new(workdir).join(".cursor");
    tokio::fs::create_dir_all(&cursor_dir).await?;
    let cfg_path = cursor_dir.join("mcp.json");
    let mut cfg: Value = match tokio::fs::read(&cfg_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
        cfg["mcpServers"] = json!({});
    }
    cfg["mcpServers"]["openab-browser"] = json!({
        "url": format!("http://{addr}/mcp"),
        "headers": { "Authorization": format!("Bearer {bearer}") }
    });
    tokio::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg)?).await?;

    Ok((addr, ct))
}

#[cfg(test)]
mod tests {
    use super::{browser_tools, spawn_mcp_server, start_session_server, BrowserTunnel, ProxyHandler};

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

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
use tracing::warn;
use std::sync::Arc;

/// Core-side interface to the browser MCP-over-ACP tunnel (D6-a'). Implemented by the ROOT
/// (which bridges to the gateway's per-connection tunnel registry) and consumed by the MCP
/// proxy here. Keeping the trait in core with the impl in root preserves the core/gateway
/// sibling independence, matching the existing `ChatAdapter` pattern.
#[async_trait::async_trait]
pub trait AcpMcpTunnel: Send + Sync {
    /// Forward an inner MCP request (e.g. `tools/call`) to the client MCP server identified by
    /// `(channel_id, server_id)` and return the inner MCP result payload. Err if no matching
    /// tunnel is currently attached to that session.
    ///
    /// `server_id` selects among multiple `type:acp` servers on one session (compound-key
    /// registry, P1). During the single-browser transition an empty `server_id` is a sentinel
    /// meaning "the sole tunnel on this channel" — the proxy/bridge callers don't yet know the
    /// client-declared id at spawn time (real per-server routing lands in P2).
    async fn call(
        &self,
        channel_id: &str,
        server_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String>;

    /// The `type:acp` servers currently registered for `channel_id`, as `(declared_name,
    /// server_id)` pairs.
    ///
    /// Both halves are needed and they are *not* interchangeable (ADR §6.1): the registry is keyed
    /// by the client-minted `server_id`, which the reference client mints as a fresh UUID **per
    /// connection**, while a tool name carries the stable declared **name** (`katashiro.click`) and
    /// the §6.4 trust gate is keyed by that name too. Enumerating both is what lets a capability
    /// source resolve a tool prefix back to a tunnel; matching a prefix against the registry key
    /// alone can never work.
    ///
    /// Sync because implementations just read an in-memory registry. The default is empty, so
    /// implementations that track no declarations (test doubles, single-target bridges) simply
    /// advertise nothing.
    fn servers(&self, _channel_id: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// The fixed set of browser tools OpenAB advertises over MCP (D4 static-advertise). DOM-
/// semantic actions the extension executes in the user's active tab; model-agnostic.
pub fn browser_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "katashiro.click",
            "Click the element matching a CSS selector in the active browser tab.",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "CSS selector" } },
                "required": ["selector"]
            })),
        ),
        Tool::new(
            "katashiro.read_dom",
            "Read a snapshot of the active tab's DOM (optionally scoped to a selector).",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "optional CSS selector to scope the snapshot" } }
            })),
        ),
        Tool::new(
            "katashiro.navigate",
            "Navigate the active browser tab to a URL.",
            object(json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "absolute URL" } },
                "required": ["url"]
            })),
        ),
        Tool::new(
            "katashiro.type",
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
            "katashiro.screenshot",
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
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
}

impl ProxyHandler {
    pub fn new(channel_id: String, tunnel: Option<Arc<dyn AcpMcpTunnel>>) -> Self {
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
        // Empty server_id sentinel (Fork A): this single-browser proxy doesn't know the
        // client-declared server id; RootBrowserTunnel resolves the sole tunnel on the channel.
        let result = tunnel
            .call(&self.channel_id, "", "tools/call", Some(params))
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
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
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
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
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

    // kiro `--agent <name>` deployments read the agent file, not settings/mcp.json —
    // merge (and allowlist) there too, or the tools never reach OAB bot agents.
    merge_kiro_agent_configs(workdir, &entry).await?;

    // On session evict/drop the caller cancels `ct`; strip our now-dead `openab-browser` entry
    // (with its live bearer) from each config so a stale credential doesn't linger. Only remove it
    // if it still points at OUR addr — a concurrent/reconnected session may have already replaced
    // it, and we must not clobber that live entry (the mcp.json paths are shared across acp: sessions).
    let cleanup_paths = cfg_paths.to_vec();
    let cleanup_url = our_url;
    let cleanup_ct = ct.clone();
    let cleanup_workdir = workdir.to_string();
    tokio::spawn(async move {
        cleanup_ct.cancelled().await;
        cleanup_kiro_agent_configs(&cleanup_workdir, &cleanup_url).await;
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

/// Merge the `openab-browser` entry into every kiro **per-agent** config
/// (`<workdir>/.kiro/agents/*.json`). When kiro-cli runs with `--agent <name>`
/// — as every OAB bot deployment does — it reads its MCP server list from the
/// agent file, NOT from `.kiro/settings/mcp.json`, and gates tools through the
/// file's `allowedTools` allowlist (verified live on the b2 fleet deployment;
/// see docs/gmail-native.md "Kiro CLI gotcha"). Without this, browser tools
/// are invisible to exactly the deployments this feature targets.
///
/// Unlike the settings-file writer, agent files carry unrelated config
/// (model, description, allowlists), so an unparseable file is SKIPPED —
/// never clobbered with a fresh object. macOS metadata droppings
/// (`._*.json`) are ignored. Missing agents dir = no-op.
async fn merge_kiro_agent_configs(workdir: &str, entry: &Value) -> std::io::Result<()> {
    let dir = std::path::Path::new(workdir).join(".kiro").join("agents");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return Ok(()); // no agents dir → nothing runs with --agent here
    };
    while let Ok(Some(f)) = rd.next_entry().await {
        let path = f.path();
        let name = f.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("._") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(mut cfg) = serde_json::from_slice::<Value>(&bytes) else {
            continue; // agent files carry model/allowlists — never clobber
        };
        if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
            cfg["mcpServers"] = json!({});
        }
        let mut changed = false;
        if cfg["mcpServers"]["openab-browser"] != *entry {
            cfg["mcpServers"]["openab-browser"] = entry.clone();
            changed = true;
        }
        // `allowedTools` is a default-deny allowlist: adding the server
        // without allowlisting it leaves every browser tool blocked.
        if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
            if !allowed.iter().any(|v| v.as_str() == Some("@openab-browser")) {
                allowed.push(json!("@openab-browser"));
                changed = true;
            }
        }
        if changed {
            write_private(&path, &serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    Ok(())
}

/// Session-evict counterpart of [`merge_kiro_agent_configs`]: strip the
/// now-dead `openab-browser` entry (and its `allowedTools` grant) from every
/// kiro agent file — but only when the entry still points at OUR `url`, so a
/// concurrent/reconnected session's live entry is never clobbered (same rule
/// as the settings-file cleanup). Static (url-less) bridge entries are left
/// alone.
async fn cleanup_kiro_agent_configs(workdir: &str, url: &str) {
    let dir = std::path::Path::new(workdir).join(".kiro").join("agents");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(f)) = rd.next_entry().await {
        let path = f.path();
        let name = f.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("._") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(mut cfg) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let still_ours = cfg
            .pointer("/mcpServers/openab-browser/url")
            .and_then(Value::as_str)
            == Some(url);
        if !still_ours {
            continue;
        }
        if let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) {
            servers.remove("openab-browser");
        }
        if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
            allowed.retain(|v| v.as_str() != Some("@openab-browser"));
        }
        if let Ok(out) = serde_json::to_vec_pretty(&cfg) {
            let _ = write_private(&path, &out).await;
        }
    }
}

/// Write the STATIC, write-once `openab-browser` bridge entry into each colocated CLI's mcp.json
/// (Option C, bridge mode). Unlike the per-session HTTP proxy config, this carries no port/bearer
/// — it is the same `{command:"openab", args:["browser-bridge"]}` for every session, so it can be
/// written once and never goes stale. That fixes the shared-config clobber the per-session dynamic
/// write suffers when several sessions of one agent share a single mcp.json. Merges without
/// touching the user's other servers; idempotent (a no-op when already present + identical).
pub async fn write_bridge_mcp_config(workdir: &str) -> std::io::Result<()> {
    // Pure static entry — byte-identical for every session (idempotent, no cross-session clobber).
    // The channel is deliberately NOT carried here: the MCP client scrubs the server's env and its
    // config-var expansion is vendor-specific, so the `openab browser-bridge` shim resolves its OWN
    // channel by walking up to the agent process (Option C b2). This entry never goes stale.
    let entry = json!({ "command": "openab", "args": ["browser-bridge"] });
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
        // Idempotent: only rewrite when absent or changed (no needless mtime churn each session).
        if cfg["mcpServers"]["openab-browser"] != entry {
            cfg["mcpServers"]["openab-browser"] = entry.clone();
            tokio::fs::write(cfg_path, serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    // kiro `--agent` deployments read agent files, not settings/mcp.json (same
    // gap as the proxy-mode writer). The static entry is idempotent there too.
    merge_kiro_agent_configs(workdir, &entry).await?;
    Ok(())
}

/// Write the STATIC, write-once `openab` facade entry into each colocated CLI's MCP config
/// (Facade mode). Like the Option C bridge entry it is byte-identical for every session —
/// the per-session secret is NOT in the file: the entry references the
/// `OPENAB_SESSION_TOKEN` environment variable, which the pool injects into each spawned
/// agent process (config-var expansion is exactly how deployed agents already reference
/// per-bot secrets). No cross-session clobber, nothing to clean up on evict — the token
/// dies with the agent process and its registry entry.
/// True when an `openab-browser` entry is one we can **prove** we wrote, and so may be dropped
/// when facade mode takes over.
///
/// Only the bridge entry qualifies. It is byte-identical every session
/// (`{"command":"openab","args":["browser-bridge"]}`) and names our own binary and subcommand, so
/// matching it is itself the proof.
///
/// The per-session proxy entry deliberately does **not** qualify, correcting an earlier version of
/// this function. Its url and bearer are minted per session and never recorded anywhere, so
/// "loopback url plus some `Bearer` header" is a description, not an identity — it matches any
/// local MCP server an operator happened to configure under this key. That check claimed to
/// recognise "a bearer we minted" while comparing against nothing we had kept. With no way to
/// prove ownership we fail closed and preserve.
///
/// Preserving costs little. A leftover proxy entry names an ephemeral port belonging to a session
/// that is gone — `start_session_server` binds `127.0.0.1:0` and drops the listener with the
/// session — so it is dead configuration rather than a live bypass. The bridge entry is the one
/// that would still resolve and run, and that is the one removed.
fn is_openab_direct_browser_entry(entry: &Value) -> bool {
    entry.get("command").and_then(Value::as_str) == Some("openab")
        && entry.get("args") == Some(&json!(["browser-bridge"]))
}

/// Drop a stale direct-transport `openab-browser` entry from an `mcpServers` map, returning
/// whether anything was removed. Both entries otherwise load side by side and the model may pick
/// the direct one, bypassing the facade's policy and audit.
fn strip_direct_browser_entry(cfg: &mut Value) -> bool {
    let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    match servers.get("openab-browser") {
        Some(entry) if is_openab_direct_browser_entry(entry) => {
            servers.remove("openab-browser");
            true
        }
        _ => false,
    }
}

pub async fn write_facade_mcp_config(workdir: &str, facade_url: &str) -> std::io::Result<()> {
    let entry = json!({
        "url": facade_url,
        "headers": { "Authorization": "Bearer ${OPENAB_SESSION_TOKEN}" }
    });
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
        // Publish under "openab" (the facade), not "openab-browser": the agent
        // reaches ALL facade capabilities through this one entry.
        let mut changed = false;
        if cfg["mcpServers"]["openab"] != entry {
            cfg["mcpServers"]["openab"] = entry.clone();
            changed = true;
        }
        // Retire the direct transport we previously wrote here. Leaving it means both entries
        // load and the model can reach the browser without passing through facade policy/audit.
        changed |= strip_direct_browser_entry(&mut cfg);
        if changed {
            tokio::fs::write(cfg_path, serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    // kiro `--agent` deployments read agent files, not settings/mcp.json.
    merge_kiro_agent_facade_configs(workdir, &entry).await?;
    Ok(())
}

/// Facade-mode sibling of [`merge_kiro_agent_configs`]: merges the static
/// `openab` facade entry + `@openab` allowlist grant into every
/// `.kiro/agents/*.json`. Same never-clobber rules; nothing to clean up on
/// evict (the entry is static and the token lives in the process env).
async fn merge_kiro_agent_facade_configs(workdir: &str, entry: &Value) -> std::io::Result<()> {
    let dir = std::path::Path::new(workdir).join(".kiro").join("agents");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return Ok(());
    };
    while let Ok(Some(f)) = rd.next_entry().await {
        let path = f.path();
        let name = f.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("._") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(mut cfg) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
            cfg["mcpServers"] = json!({});
        }
        let mut changed = false;
        if cfg["mcpServers"]["openab"] != *entry {
            cfg["mcpServers"]["openab"] = entry.clone();
            changed = true;
        }
        // Same retirement as the settings files, plus the agent-file allowlist grant that made
        // the direct server callable — `allowedTools` is default-deny, so a leftover
        // `@openab-browser` is what keeps the bypass reachable here.
        if strip_direct_browser_entry(&mut cfg) {
            changed = true;
            if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
                allowed.retain(|v| v.as_str() != Some("@openab-browser"));
            }
        }
        if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
            if !allowed.iter().any(|v| v.as_str() == Some("@openab")) {
                allowed.push(json!("@openab"));
                changed = true;
            }
        }
        if changed {
            write_private(&path, &serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    Ok(())
}

/// Broker-side session credential hook (Facade mode). Implemented by the root
/// (closing over the facade's `SessionTokens` registry — core stays free of
/// the openab-mcp dependency); the pool calls it at session spawn/evict.
pub trait SessionTokenRegistrar: Send + Sync {
    /// Mint (or re-mint) the token for `channel_id`; returns the value the
    /// pool injects as `OPENAB_SESSION_TOKEN` in the agent's environment.
    fn mint(&self, channel_id: &str) -> String;
    /// Revoke one specific token (the session that held it was evicted).
    ///
    /// Deliberately keyed by token, not by channel. `mint` replaces whatever token a channel had,
    /// so a replaced session's teardown runs *after* its successor has already minted a new one;
    /// revoking by channel would strip that live token and silently cut the new agent off from the
    /// facade. Revoking the exact token makes a late teardown a no-op instead (review R1).
    fn revoke(&self, token: &str);
}

/// Selected browser transport for the Option C rollout. `OPENAB_BROWSER_MODE=bridge` opts into
/// the stdio bridge; anything else (including unset) keeps the per-session HTTP proxy — the safe
/// default during rollout, so existing Cursor/Kiro browser control is unchanged until flipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// Browser tools served through the OAB MCP Facade as a session-aware
    /// in-process capability source (one listener, session identity via
    /// broker-minted tokens). The default when the facade is running;
    /// falls back to `Proxy` when it is not (no `[mcp]` in config).
    Facade,
    /// Per-session loopback HTTP MCP server + dynamic config (the original
    /// default; explicit opt-out from facade routing).
    Proxy,
    Bridge,
}

impl BrowserMode {
    pub fn is_bridge(self) -> bool {
        matches!(self, BrowserMode::Bridge)
    }
}

fn parse_browser_mode(s: Option<&str>) -> BrowserMode {
    match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("bridge") => BrowserMode::Bridge,
        Some("proxy") => BrowserMode::Proxy,
        _ => BrowserMode::Facade,
    }
}

/// Read the browser transport mode from `OPENAB_BROWSER_MODE` (default: proxy).
pub fn browser_mode() -> BrowserMode {
    parse_browser_mode(std::env::var("OPENAB_BROWSER_MODE").ok().as_deref())
}

/// Per-pod browser-bridge socket path (overridable via `OPENAB_BROWSER_SOCKET`). Single source of
/// truth shared by the core socket server and the `openab browser-bridge` shim so they agree.
pub fn browser_socket_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("OPENAB_BROWSER_SOCKET") {
        return p.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".into());
    std::path::Path::new(&home).join(".openab").join("browser.sock")
}

// ---- Option C: per-pod stdio-bridge socket server -------------------------------------------
// A single unix socket per pod multiplexes ALL sessions. The `openab browser-bridge` shim
// (spawned per agent session by the CLI's MCP client) connects and forwards inner MCP requests
// tagged with its own `channel_id` (from the OPENAB_BROWSER_CHANNEL env it inherits); core routes
// `tools/call` to that session's AcpMcpTunnel. This is the stable, variant-agnostic replacement
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
    tunnel: &Option<Arc<dyn AcpMcpTunnel>>,
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
            // Empty server_id sentinel (Fork A): the bridge frame carries no server id yet;
            // RootBrowserTunnel resolves the sole tunnel on the channel. Real per-server routing
            // (server_id in the frame) lands in P2.
            Some(t) => match t
                .call(channel_id, "", "tools/call", request.get("params").cloned())
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
/// Extract `OPENAB_BROWSER_CHANNEL` from a null-separated `/proc/<pid>/environ` blob.
fn parse_channel_from_environ(bytes: &[u8]) -> Option<String> {
    for kv in bytes.split(|b| *b == 0) {
        if let Some(rest) = kv.strip_prefix(b"OPENAB_BROWSER_CHANNEL=") {
            let v = String::from_utf8_lossy(rest).into_owned();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Parse the parent PID from a `/proc/<pid>/stat` line. Field 2 (`comm`) is parenthesized and may
/// contain spaces or `)`, so split after the LAST `)`: the remainder is "state ppid pgrp ...".
fn parse_ppid_from_stat(stat: &str) -> Option<u32> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(1)?.parse().ok()
}

/// Walk up from `start_pid` and return the first ancestor's `OPENAB_BROWSER_CHANNEL`.
///
/// This is the **authoritative** channel for a bridge connection: the agent process openab
/// spawned carries the variable, and the shim it spawns is always a descendant. Deriving it from
/// a kernel-supplied peer pid means a caller cannot choose which session it drives — unlike the
/// `channel_id` in the frame, which is merely a claim (review R2).
pub fn channel_from_process_ancestry(start_pid: u32) -> Option<String> {
    let mut pid = start_pid;
    for _ in 0..16 {
        if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/environ")) {
            if let Some(c) = parse_channel_from_environ(&bytes) {
                return Some(c);
            }
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        match parse_ppid_from_stat(&stat) {
            Some(ppid) if ppid > 1 => pid = ppid, // step up (stop at init/tini = 1)
            _ => break,
        }
    }
    None
}

/// Maps a connecting peer's pid to the channel it is allowed to drive. Injectable so the socket
/// server can be tested without a real agent process tree above the test binary.
pub type ChannelResolver = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;

/// Hard ceiling for one bridge frame (review R4).
///
/// Matches the ACP tunnel's own frame ceiling so the bridge is never the tighter bottleneck for
/// legitimate MCP traffic, while still bounding what a single frame can make us allocate.
const MAX_BRIDGE_FRAME_BYTES: usize = 8 << 20; // 8 MiB

/// Read one newline-terminated frame, refusing to buffer more than `max` bytes.
///
/// `BufReader::lines()` grows until it sees a newline, so a peer that never sends one pins an
/// arbitrarily large allocation — no malice required, a wedged writer does it too. Returns
/// `Ok(None)` at EOF, and `InvalidData` once the pending frame would exceed `max`; the caller
/// drops the connection rather than trying to resynchronise mid-frame.
async fn read_frame_bounded<R>(reader: &mut R, max: usize) -> std::io::Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut out: Vec<u8> = Vec::new();
    loop {
        // Copy out of the fill buffer before consuming: `available` borrows the reader.
        let (chunk, terminated) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return Ok((!out.is_empty()).then_some(out)); // EOF
            }
            match available.iter().position(|b| *b == b'\n') {
                Some(pos) => (available[..pos].to_vec(), true),
                None => (available.to_vec(), false),
            }
        };
        if out.len() + chunk.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bridge frame exceeds {max} bytes"),
            ));
        }
        out.extend_from_slice(&chunk);
        reader.consume(chunk.len() + usize::from(terminated));
        if terminated {
            return Ok(Some(out));
        }
    }
}

pub async fn serve_browser_socket(
    path: std::path::PathBuf,
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
    ct: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    let resolver: ChannelResolver = Arc::new(channel_from_process_ancestry);
    serve_browser_socket_with_resolver(path, tunnel, resolver, ct).await
}

/// [`serve_browser_socket`] with an injectable peer→channel resolver (tests).
pub async fn serve_browser_socket_with_resolver(
    path: std::path::PathBuf,
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
    resolver: ChannelResolver,
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
                            tokio::spawn(handle_browser_conn(
                                stream,
                                tunnel.clone(),
                                resolver.clone(),
                            ));
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

/// Start the browser socket for the process lifetime (no external cancellation handle) — used by
/// the broker in bridge mode. The pod-wide server lives as long as the process, so no caller-side
/// tokio-util dependency is needed.
pub async fn serve_browser_socket_forever(
    path: std::path::PathBuf,
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
) -> std::io::Result<()> {
    serve_browser_socket(path, tunnel, tokio_util::sync::CancellationToken::new()).await
}

async fn handle_browser_conn(
    stream: tokio::net::UnixStream,
    tunnel: Option<Arc<dyn AcpMcpTunnel>>,
    resolver: ChannelResolver,
) {
    use tokio::io::{AsyncWriteExt, BufReader};

    // Authenticate the CONNECTION, not the frame (review R2). 0600 on the socket only proves the
    // peer shares our uid; it does not say which session the peer belongs to. The `channel_id` in
    // a frame is a claim the sender chooses, so trusting it let any same-uid process drive another
    // live session's browser. Derive the channel from the kernel-supplied peer pid instead, and
    // refuse the connection outright when it cannot be established — an unauthenticated peer gets
    // no session at all rather than a default one.
    let peer_pid = stream.peer_cred().ok().and_then(|c| c.pid());
    let Some(authenticated_channel) = peer_pid.and_then(|pid| resolver(pid as u32)) else {
        warn!(
            peer_pid = ?peer_pid,
            "browser bridge: refusing a connection whose browser channel could not be established"
        );
        return;
    };

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let line = match read_frame_bounded(&mut reader, MAX_BRIDGE_FRAME_BYTES).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break, // EOF
            Err(e) => {
                // Oversized or unreadable: the stream cannot be resynchronised mid-frame, so the
                // connection goes rather than leaving a partial frame buffered (R4).
                warn!(peer_pid = ?peer_pid, error = %e, "browser bridge: dropping connection");
                break;
            }
        };
        let Ok(line) = String::from_utf8(line) else {
            continue; // skip a non-UTF8 frame rather than drop the connection
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue; // skip a malformed frame rather than drop the connection
        };
        // A frame may still carry channel_id (the shim sends it), but it is only ever checked
        // against the authenticated value — never used to select a session.
        if let Some(claimed) = frame.get("channel_id").and_then(Value::as_str) {
            if !claimed.is_empty() && claimed != authenticated_channel {
                warn!(
                    peer_pid = ?peer_pid,
                    claimed,
                    "browser bridge: frame claimed a channel this peer does not own; dropping"
                );
                continue;
            }
        }
        let Some(request) = frame.get("request") else {
            continue;
        };
        if let Some(resp) = dispatch_browser_mcp(&authenticated_channel, request, &tunnel).await {
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
        browser_tools, cleanup_kiro_agent_configs, dispatch_browser_mcp,
        is_openab_direct_browser_entry, merge_kiro_agent_configs, parse_browser_mode,
        serve_browser_socket_with_resolver, spawn_mcp_server, start_session_server,
        write_bridge_mcp_config,
        write_facade_mcp_config, AcpMcpTunnel, BrowserMode, ProxyHandler,
    };

    // --- F4: facade setup retires the direct transport it replaces ---

    /// The bridge and per-session-proxy entries we wrote are recognised; anything else under the
    /// same key is not ours to delete.
    #[test]
    fn only_our_own_direct_browser_shapes_are_recognised() {
        let bridge = serde_json::json!({ "command": "openab", "args": ["browser-bridge"] });
        assert!(is_openab_direct_browser_entry(&bridge));

        // Not provably ours. The loopback+bearer shapes are the important ones: they describe our
        // old proxy entry, but they equally describe an operator's own local MCP server, and the
        // per-session url/bearer were never recorded, so ownership cannot be established.
        for foreign in [
            serde_json::json!({ "url": "http://127.0.0.1:45678/mcp", "headers": { "Authorization": "Bearer abc" } }),
            serde_json::json!({ "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer x" } }),
            serde_json::json!({ "url": "http://127.0.0.1:45678/mcp" }),
            serde_json::json!({ "command": "openab", "args": ["something-else"] }),
            serde_json::json!({ "command": "my-browser-tool", "args": ["browser-bridge"] }),
            serde_json::json!({ "url": "http://127.0.0.1:/mcp", "headers": { "Authorization": "Bearer x" } }),
        ] {
            assert!(
                !is_openab_direct_browser_entry(&foreign),
                "must not claim ownership of {foreign}"
            );
        }
    }

    #[tokio::test]
    async fn facade_setup_removes_the_stale_direct_entry_but_keeps_user_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": {
                    // ours, the bridge transport facade mode replaces
                    "openab-browser": { "command": "openab", "args": ["browser-bridge"] },
                    // the operator's own servers must survive untouched
                    "github": { "url": "http://ghpool:8080/mcp" },
                    "notes": { "command": "notes-mcp", "args": ["--stdio"] }
                },
                "someUnrelatedKey": 42
            }))
            .unwrap(),
        )
        .unwrap();

        write_facade_mcp_config(dir.path().to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        let servers = cfg["mcpServers"].as_object().unwrap();
        assert!(
            !servers.contains_key("openab-browser"),
            "the direct transport must not load alongside the facade — that is the bypass"
        );
        assert_eq!(servers["openab"]["url"], "http://127.0.0.1:8848/mcp");
        assert_eq!(servers["github"]["url"], "http://ghpool:8080/mcp");
        assert_eq!(servers["notes"]["command"], "notes-mcp");
        assert_eq!(cfg["someUnrelatedKey"], 42, "unrelated config must survive");
    }

    /// An operator's own local MCP server under this key survives facade setup — the entry **and**
    /// its allowlist grant (review R3-F2).
    ///
    /// The previous matcher treated any loopback url carrying any `Bearer` header as ours, which
    /// is precisely the shape a locally-run MCP server takes, so that configuration was deleted.
    /// Ownership of that shape cannot be proven — the per-session url and bearer were never
    /// recorded — so it is preserved now.
    #[tokio::test]
    async fn a_local_mcp_server_under_our_key_is_not_deleted() {
        let wd = tmp_workdir("r3f2").await;
        let cursor = wd.join(".cursor");
        tokio::fs::create_dir_all(&cursor).await.unwrap();
        // Indistinguishable from our retired proxy entry by shape alone.
        let theirs = serde_json::json!({
            "url": "http://127.0.0.1:45678/mcp",
            "headers": { "Authorization": "Bearer their-own-token" }
        });
        tokio::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({ "mcpServers": { "openab-browser": theirs } }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let agent = wd.join(".kiro/agents/terra.json");
        tokio::fs::write(
            &agent,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "terra",
                "mcpServers": { "openab-browser": theirs },
                "allowedTools": ["@builtin", "@openab-browser"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(cursor.join("mcp.json")).await.unwrap())
                .unwrap();
        assert_eq!(
            cfg["mcpServers"]["openab-browser"], theirs,
            "an entry we cannot prove we wrote must be preserved verbatim"
        );

        let agent_cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert_eq!(agent_cfg["mcpServers"]["openab-browser"], theirs);
        let allowed: Vec<&str> = agent_cfg["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            allowed.contains(&"@openab-browser"),
            "the grant must survive too — revoking it silently disables the operator's own server"
        );
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[tokio::test]
    async fn facade_setup_leaves_a_foreign_openab_browser_entry_alone() {
        // Same key, but a shape we never wrote: it belongs to the operator, so removing it would
        // destroy their configuration to fix a bypass that entry does not create.
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        let foreign = serde_json::json!({ "url": "https://my-own-browser.example/mcp" });
        std::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": { "openab-browser": foreign }
            }))
            .unwrap(),
        )
        .unwrap();

        write_facade_mcp_config(dir.path().to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(
            cfg["mcpServers"]["openab-browser"], foreign,
            "an entry we did not write must be preserved verbatim"
        );
    }

    #[tokio::test]
    async fn facade_setup_retires_the_direct_entry_and_its_grant_in_kiro_agent_files() {
        let wd = tmp_workdir("f4-agent").await;
        let agent = wd.join(".kiro/agents/terra.json");
        tokio::fs::write(
            &agent,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "terra",
                "mcpServers": {
                    "openab-browser": { "command": "openab", "args": ["browser-bridge"] },
                    "github": { "url": "http://ghpool:8080/mcp" }
                },
                "allowedTools": ["@builtin", "@openab-browser", "@github"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert!(!cfg["mcpServers"].as_object().unwrap().contains_key("openab-browser"));
        assert_eq!(cfg["mcpServers"]["github"]["url"], "http://ghpool:8080/mcp");
        let allowed: Vec<&str> = cfg["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !allowed.contains(&"@openab-browser"),
            "allowedTools is default-deny — a leftover grant is what keeps the bypass reachable"
        );
        assert!(allowed.contains(&"@openab"), "the facade must be granted");
        assert!(allowed.contains(&"@github"), "unrelated grants must survive");
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// Unique throwaway workdir with a `.kiro/agents/` tree.
    async fn tmp_workdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oab-mcp-proxy-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(dir.join(".kiro").join("agents"))
            .await
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn kiro_agent_merge_adds_server_and_allowlist_preserving_the_rest() {
        let wd = tmp_workdir("merge").await;
        let agent = wd.join(".kiro/agents/terra.json");
        tokio::fs::write(
            &agent,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "terra",
                "model": "gpt-5.6-terra",
                "mcpServers": { "github": { "url": "http://ghpool:8080/mcp" } },
                "allowedTools": ["@builtin", "@github"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let entry = serde_json::json!({
            "url": "http://127.0.0.1:45678/mcp",
            "headers": { "Authorization": "Bearer tok" }
        });
        merge_kiro_agent_configs(wd.to_str().unwrap(), &entry)
            .await
            .unwrap();
        let cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert_eq!(cfg["mcpServers"]["openab-browser"], entry);
        assert_eq!(
            cfg["mcpServers"]["github"]["url"], "http://ghpool:8080/mcp",
            "pre-existing servers must be preserved"
        );
        assert_eq!(cfg["model"], "gpt-5.6-terra", "unrelated fields preserved");
        let allowed = cfg["allowedTools"].as_array().unwrap();
        assert!(
            allowed.iter().any(|v| v == "@openab-browser"),
            "allowedTools is default-deny — the server must be allowlisted: {allowed:?}"
        );
        // Idempotent: second merge changes nothing (byte-stable allowlist).
        merge_kiro_agent_configs(wd.to_str().unwrap(), &entry)
            .await
            .unwrap();
        let cfg2: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert_eq!(cfg, cfg2);
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[tokio::test]
    async fn kiro_agent_merge_skips_unparseable_and_metadata_files() {
        let wd = tmp_workdir("skip").await;
        let junk = wd.join(".kiro/agents/broken.json");
        tokio::fs::write(&junk, b"{not json").await.unwrap();
        let meta = wd.join(".kiro/agents/._terra.json");
        tokio::fs::write(&meta, b"\x00\x05\x16\x07").await.unwrap();
        let entry = serde_json::json!({ "url": "http://127.0.0.1:1/mcp" });
        merge_kiro_agent_configs(wd.to_str().unwrap(), &entry)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(&junk).await.unwrap(),
            b"{not json",
            "unparseable agent files must be skipped, never clobbered"
        );
        assert_eq!(tokio::fs::read(&meta).await.unwrap(), b"\x00\x05\x16\x07");
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[tokio::test]
    async fn kiro_agent_merge_without_agents_dir_is_noop() {
        let wd = std::env::temp_dir().join(format!(
            "oab-mcp-proxy-test-noop-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&wd).await.unwrap();
        merge_kiro_agent_configs(
            wd.to_str().unwrap(),
            &serde_json::json!({ "url": "http://127.0.0.1:1/mcp" }),
        )
        .await
        .unwrap();
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[tokio::test]
    async fn kiro_agent_cleanup_removes_only_our_url_and_its_grant() {
        let wd = tmp_workdir("cleanup").await;
        let ours = wd.join(".kiro/agents/ours.json");
        tokio::fs::write(
            &ours,
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": { "openab-browser": { "url": "http://127.0.0.1:1111/mcp" } },
                "allowedTools": ["@builtin", "@openab-browser"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let foreign = wd.join(".kiro/agents/foreign.json");
        tokio::fs::write(
            &foreign,
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": { "openab-browser": { "url": "http://127.0.0.1:2222/mcp" } },
                "allowedTools": ["@openab-browser"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        cleanup_kiro_agent_configs(wd.to_str().unwrap(), "http://127.0.0.1:1111/mcp").await;
        let ours_cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&ours).await.unwrap()).unwrap();
        assert!(ours_cfg["mcpServers"]["openab-browser"].is_null());
        assert!(
            !ours_cfg["allowedTools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "@openab-browser"),
            "the stale allowlist grant must be revoked with the entry"
        );
        let foreign_cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&foreign).await.unwrap()).unwrap();
        assert_eq!(
            foreign_cfg["mcpServers"]["openab-browser"]["url"], "http://127.0.0.1:2222/mcp",
            "a concurrent session's live entry must never be clobbered"
        );
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[test]
    fn browser_mode_defaults_to_facade_with_proxy_and_bridge_opt_outs() {
        // Facade is the default: browser tools ride the OAB MCP Facade as a
        // session-aware source (falls back to Proxy at runtime when no
        // facade is serving — see the pool's mode fallback).
        assert_eq!(parse_browser_mode(None), BrowserMode::Facade);
        assert_eq!(parse_browser_mode(Some("")), BrowserMode::Facade);
        assert_eq!(parse_browser_mode(Some("junk")), BrowserMode::Facade);
        // Explicit opt-outs keep their exact prior semantics.
        assert_eq!(parse_browser_mode(Some("proxy")), BrowserMode::Proxy);
        assert_eq!(parse_browser_mode(Some("bridge")), BrowserMode::Bridge);
        assert_eq!(parse_browser_mode(Some("  Bridge  ")), BrowserMode::Bridge);
        assert!(BrowserMode::Bridge.is_bridge());
        assert!(!BrowserMode::Proxy.is_bridge());
        assert!(!BrowserMode::Facade.is_bridge());
    }

    struct MockTunnel;
    #[async_trait::async_trait]
    impl AcpMcpTunnel for MockTunnel {
        async fn call(
            &self,
            channel_id: &str,
            server_id: &str,
            method: &str,
            _params: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            assert_eq!(channel_id, "acp_x");
            assert_eq!(server_id, ""); // proxy passes the empty sentinel (Fork A)
            assert_eq!(method, "tools/call");
            Ok(serde_json::json!({"content": [{"type": "text", "text": "clicked"}]}))
        }
    }

    #[tokio::test]
    async fn call_tool_forwards_to_the_tunnel() {
        let h = ProxyHandler::new("acp_x".into(), Some(std::sync::Arc::new(MockTunnel)));
        let result = h.forward_tool_call("katashiro.click", None).await.unwrap();
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["content"][0]["text"], serde_json::json!("clicked"));
    }

    #[tokio::test]
    async fn call_tool_reports_not_connected_without_a_tunnel() {
        let h = ProxyHandler::new("acp_x".into(), None);
        assert!(
            h.forward_tool_call("katashiro.click", None).await.is_err(),
            "a call with no attached browser must error (D4)"
        );
    }

    // --- Option C: browser-bridge socket dispatch ---
    struct RecordTunnel {
        result: serde_json::Value,
    }
    #[async_trait::async_trait]
    impl AcpMcpTunnel for RecordTunnel {
        async fn call(
            &self,
            channel_id: &str,
            _server_id: &str,
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
    impl AcpMcpTunnel for ErrTunnel {
        async fn call(
            &self,
            _c: &str,
            _s: &str,
            _m: &str,
            _p: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            Err("no browser attached".into())
        }
    }
    fn req(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }
    fn arc_tunnel<T: AcpMcpTunnel + 'static>(t: T) -> Option<std::sync::Arc<dyn AcpMcpTunnel>> {
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
            &req(3, "tools/call", serde_json::json!({ "name": "katashiro.read_dom", "arguments": {} })),
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
            &req(4, "tools/call", serde_json::json!({ "name": "katashiro.click" })),
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
            &req(5, "tools/call", serde_json::json!({ "name": "katashiro.click" })),
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
    async fn write_bridge_config_writes_static_entry_to_both_variants() {
        let dir = tempfile::tempdir().unwrap();
        write_bridge_mcp_config(dir.path().to_str().unwrap())
            .await
            .unwrap();
        for rel in [".cursor/mcp.json", ".kiro/settings/mcp.json"] {
            let cfg: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join(rel)).unwrap()).unwrap();
            let e = &cfg["mcpServers"]["openab-browser"];
            assert_eq!(e["command"], "openab");
            assert_eq!(e["args"], serde_json::json!(["browser-bridge"]));
            assert!(
                e.get("env").is_none(),
                "channel is resolved by the shim (b2), not carried in config"
            );
            assert!(e.get("url").is_none(), "bridge entry carries no url/port");
            assert!(e.get("headers").is_none(), "bridge entry carries no bearer");
        }
    }

    #[tokio::test]
    async fn write_bridge_config_merges_without_clobber_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(
            cursor.join("mcp.json"),
            r#"{"mcpServers":{"other":{"url":"http://x"}}}"#,
        )
        .unwrap();
        let wd = dir.path().to_str().unwrap();
        write_bridge_mcp_config(wd).await.unwrap();
        write_bridge_mcp_config(wd).await.unwrap(); // idempotent second call
        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(cfg["mcpServers"]["other"]["url"], "http://x"); // user's server preserved
        assert_eq!(cfg["mcpServers"]["openab-browser"]["command"], "openab");
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
        // The peer here is the test binary, whose ancestry carries no channel, so inject the
        // resolver the way a real deployment's process tree would answer.
        serve_browser_socket_with_resolver(
            sock.clone(),
            tunnel,
            std::sync::Arc::new(|_pid| Some("acp_win1".to_string())),
            ct.clone(),
        )
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
            "request": req(9, "tools/call", serde_json::json!({ "name": "katashiro.read_dom", "arguments": {} }))
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

    // --- R4: one frame cannot pin an unbounded allocation ---

    /// `BufReader::lines()` grows until it sees a newline, so a peer that never sends one — a
    /// wedged writer just as much as a hostile one — pins an arbitrarily large buffer. Tested at a
    /// small cap so the assertion is about the bound, not about allocating megabytes.
    #[tokio::test]
    async fn an_unterminated_frame_is_refused_once_it_passes_the_cap() {
        // Terminated frames under the cap read normally, newline consumed.
        let mut r = std::io::Cursor::new(b"hello\nworld\n".to_vec());
        assert_eq!(
            super::read_frame_bounded(&mut r, 16).await.unwrap(),
            Some(b"hello".to_vec())
        );
        assert_eq!(
            super::read_frame_bounded(&mut r, 16).await.unwrap(),
            Some(b"world".to_vec())
        );
        // EOF is None, not an error.
        assert_eq!(super::read_frame_bounded(&mut r, 16).await.unwrap(), None);

        // Exactly at the cap is still allowed — the bound is inclusive.
        let mut at_cap = std::io::Cursor::new(b"0123456789abcdef\n".to_vec());
        assert_eq!(
            super::read_frame_bounded(&mut at_cap, 16).await.unwrap(),
            Some(b"0123456789abcdef".to_vec())
        );

        // Unterminated and over the cap: refused rather than buffered.
        let mut flood = std::io::Cursor::new(vec![b'x'; 64]);
        let err = super::read_frame_bounded(&mut flood, 16)
            .await
            .expect_err("an unterminated frame past the cap must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // A terminated frame that is merely too long is refused too.
        let mut long = std::io::Cursor::new([vec![b'y'; 64], vec![b'\n']].concat());
        assert!(super::read_frame_bounded(&mut long, 16).await.is_err());
    }

    // --- R2: the socket authenticates the connection, not the frame ---

    #[test]
    fn ancestry_parsers_read_proc_shapes() {
        assert_eq!(super::parse_ppid_from_stat("834 (sh) S 25 834 25 0 -1 ..."), Some(25));
        assert_eq!(super::parse_ppid_from_stat("658 (cursor agent) R 25 658 ..."), Some(25));
        // ')' inside comm — split after the LAST ')'
        assert_eq!(super::parse_ppid_from_stat("5 (weird )proc) S 3 5 ..."), Some(3));
        assert_eq!(super::parse_ppid_from_stat("nonsense"), None);

        let env = b"HOME=/h\0OPENAB_BROWSER_CHANNEL=acp_xyz\0PATH=/x\0";
        assert_eq!(
            super::parse_channel_from_environ(env).as_deref(),
            Some("acp_xyz")
        );
        assert_eq!(super::parse_channel_from_environ(b"HOME=/x\0PATH=/y\0"), None);
        assert_eq!(super::parse_channel_from_environ(b"OPENAB_BROWSER_CHANNEL=\0"), None);
    }

    /// A same-uid peer must not be able to drive a session it does not own by naming it. The
    /// socket's 0600 mode only proves same-uid; the channel comes from the peer's process
    /// ancestry, and a frame claiming a different one is dropped rather than honoured.
    #[tokio::test]
    async fn a_frame_cannot_claim_a_channel_the_peer_does_not_own() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("browser.sock");
        let tunnel = arc_tunnel(RecordTunnel {
            result: serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] }),
        });
        let ct = tokio_util::sync::CancellationToken::new();
        // This peer owns acp_win1 (RecordTunnel asserts it only ever sees that channel).
        serve_browser_socket_with_resolver(
            sock.clone(),
            tunnel,
            std::sync::Arc::new(|_pid| Some("acp_win1".to_string())),
            ct.clone(),
        )
        .await
        .unwrap();
        let stream = loop {
            match tokio::net::UnixStream::connect(&sock).await {
                Ok(s) => break s,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        let (rd, mut wr) = stream.into_split();

        // Claim someone else's session first, then a legitimate frame.
        for (channel, id) in [("acp_victim", 1), ("acp_win1", 2)] {
            let frame = serde_json::json!({
                "channel_id": channel,
                "request": req(id, "tools/call", serde_json::json!({ "name": "katashiro.read_dom", "arguments": {} }))
            });
            let mut line = serde_json::to_vec(&frame).unwrap();
            line.push(b'\n');
            wr.write_all(&line).await.unwrap();
        }

        // Only the legitimate frame is answered; the spoofed one produced no reply at all.
        let mut resp = String::new();
        BufReader::new(rd).read_line(&mut resp).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["id"], 2,
            "the first response must be for the legitimate frame — the spoofed channel was dropped"
        );
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
                "katashiro.click",
                "katashiro.read_dom",
                "katashiro.navigate",
                "katashiro.type",
                "katashiro.screenshot"
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

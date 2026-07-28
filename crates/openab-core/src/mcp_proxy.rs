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

/// Write the STATIC, write-once `openab` facade entry into each colocated CLI's MCP config
/// (Facade mode). Like the Option C bridge entry it is byte-identical for every session —
/// the per-session secret is NOT in the file: the entry references the
/// `OPENAB_SESSION_TOKEN` environment variable, which the pool injects into each spawned
/// agent process (config-var expansion is exactly how deployed agents already reference
/// per-bot secrets). No cross-session clobber, nothing to clean up on evict — the token
/// dies with the agent process and its registry entry.
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

/// Selected browser transport. `OPENAB_BROWSER_MODE=proxy` opts out of facade routing; anything
/// else, including unset, uses the facade.
///
/// The stdio bridge was the third variant and is gone. `bridge` is now simply an unrecognised
/// value and falls through to `Facade` like any other — deliberately, so a deployment still
/// carrying `OPENAB_BROWSER_MODE=bridge` from the previous release comes up with working browser
/// control rather than refusing to start on a value that used to be valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// Browser tools served through the OAB MCP Facade as a session-aware
    /// in-process capability source (one listener, session identity via
    /// broker-minted tokens). The default when the facade is running;
    /// falls back to `Proxy` when it is not (no `[mcp]` in config).
    Facade,
    /// Per-session loopback HTTP MCP server + dynamic config (explicit opt-out
    /// from facade routing).
    Proxy,
}

fn parse_browser_mode(s: Option<&str>) -> BrowserMode {
    match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("proxy") => BrowserMode::Proxy,
        _ => BrowserMode::Facade,
    }
}

/// True when `OPENAB_BROWSER_MODE` is set to something that is not a transport we still have.
///
/// Empty and unset are not "unrecognised" — they mean "no preference expressed". Only a value the
/// operator deliberately wrote and that no longer selects anything counts, which today is `bridge`
/// and any typo.
fn is_unrecognised_mode(raw: Option<&str>) -> Option<&str> {
    let v = raw?.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("proxy") || v.eq_ignore_ascii_case("facade") {
        return None;
    }
    Some(v)
}

/// Resolve the transport actually in use from the configured value and whether the facade is
/// really serving. Pure, so the resolution is testable without touching process env.
///
/// Two separate demotions land here and both are silent to the operator on their own: an
/// unrecognised value falls through to `Facade`, and `Facade` falls back to `Proxy` when no
/// `[mcp]` wired a registrar. Composing them is how `OPENAB_BROWSER_MODE=bridge` ends up running
/// **proxy** — which is why the caller warns with the resolved value rather than the requested one.
fn resolve_browser_mode(raw: Option<&str>, facade_available: bool) -> BrowserMode {
    match parse_browser_mode(raw) {
        BrowserMode::Facade if !facade_available => BrowserMode::Proxy,
        m => m,
    }
}

/// The transport to use for this process, resolved against whether the facade is serving.
///
/// `facade_available` is false when no `[mcp]` section wired a session registrar or facade url.
///
/// Warns once per process when `OPENAB_BROWSER_MODE` names a transport that no longer exists.
/// Accepting the stale value keeps an upgraded deployment running; accepting it *silently* would
/// leave the operator believing they are on a transport that was deleted, so the warning names the
/// transport actually in use — not the one they asked for, and not merely the one that parsing
/// picked, since the `[mcp]` fallback can demote that again.
pub fn browser_mode_effective(facade_available: bool) -> BrowserMode {
    let raw = std::env::var("OPENAB_BROWSER_MODE").ok();
    let mode = resolve_browser_mode(raw.as_deref(), facade_available);
    if let Some(requested) = is_unrecognised_mode(raw.as_deref()) {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            tracing::warn!(
                requested,
                effective = ?mode,
                "OPENAB_BROWSER_MODE names a transport that no longer exists (the stdio bridge was \
                 removed); continuing on the transport shown as `effective`. Unset the variable, \
                 or set it to `proxy`, to make the configuration say what is actually running."
            );
        });
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::{
        browser_tools, cleanup_kiro_agent_configs, is_openab_direct_browser_entry,
        is_unrecognised_mode, merge_kiro_agent_configs, parse_browser_mode, resolve_browser_mode,
        spawn_mcp_server, start_session_server,
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
        // The one remaining explicit opt-out keeps its exact prior semantics.
        assert_eq!(parse_browser_mode(Some("proxy")), BrowserMode::Proxy);
        assert_eq!(parse_browser_mode(Some("  Proxy  ")), BrowserMode::Proxy);
        // `bridge` was a third mode and is gone. It must degrade to the default like any other
        // unknown value rather than being special-cased into an error: a deployment still carrying
        // OPENAB_BROWSER_MODE=bridge from the previous release has to come up with working browser
        // control, not refuse to start on a value that used to be valid.
        assert_eq!(parse_browser_mode(Some("bridge")), BrowserMode::Facade);
        assert_eq!(parse_browser_mode(Some("  Bridge  ")), BrowserMode::Facade);
    }

    /// The two demotions compose, and the second one is the reason the warning cannot just echo
    /// what parsing returned: `bridge` degrades to Facade, and Facade degrades again to Proxy when
    /// no `[mcp]` is configured. An operator who set `bridge` and has no facade is running
    /// **proxy** — the transport furthest from what they wrote.
    #[test]
    fn a_removed_mode_resolves_through_both_demotions_to_the_transport_actually_running() {
        assert_eq!(
            resolve_browser_mode(Some("bridge"), false),
            BrowserMode::Proxy,
            "bridge + no [mcp] must resolve to proxy, which is what the operator is really running"
        );
        assert_eq!(resolve_browser_mode(Some("bridge"), true), BrowserMode::Facade);
        // An explicit opt-out is not a fallback and is never re-resolved.
        assert_eq!(resolve_browser_mode(Some("proxy"), true), BrowserMode::Proxy);
        assert_eq!(resolve_browser_mode(Some("proxy"), false), BrowserMode::Proxy);
        // Unset behaves like any other non-preference.
        assert_eq!(resolve_browser_mode(None, true), BrowserMode::Facade);
        assert_eq!(resolve_browser_mode(None, false), BrowserMode::Proxy);
    }

    /// Only a value the operator actually wrote and that no longer selects anything is worth
    /// warning about. Warning on unset would fire for every default deployment, and warning on a
    /// live value would train operators to ignore it.
    #[test]
    fn only_a_deliberately_set_dead_value_is_reported_as_unrecognised() {
        assert_eq!(is_unrecognised_mode(Some("bridge")), Some("bridge"));
        assert_eq!(is_unrecognised_mode(Some("  Bridge  ")), Some("Bridge"));
        assert_eq!(is_unrecognised_mode(Some("typo")), Some("typo"));
        assert_eq!(is_unrecognised_mode(None), None);
        assert_eq!(is_unrecognised_mode(Some("")), None);
        assert_eq!(is_unrecognised_mode(Some("   ")), None);
        assert_eq!(is_unrecognised_mode(Some("proxy")), None);
        assert_eq!(is_unrecognised_mode(Some("FACADE")), None);
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

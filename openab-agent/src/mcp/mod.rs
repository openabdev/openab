//! Native MCP client. See `docs/adr/openab-agent-mcp.md`.

pub mod config;
pub mod flow;
pub mod meta_tool;
pub mod oauth;
pub mod runtime;

use serde_json::json;

use crate::llm::ToolDef;
use config::{McpConfig, ServerConfig};

pub use runtime::McpRuntimeManager;

/// Shared tool name used by `mcp_tool_def()` and the agent dispatch arm —
/// keeps the implicit contract between the two call sites explicit.
pub const MCP_TOOL_NAME: &str = "mcp";

/// The single `mcp` tool definition the LLM sees (ADR §5.2). The schema is
/// intentionally permissive on the per-action fields — the LLM should call
/// `mcp(action="help")` first to learn the action-specific contract.
pub fn mcp_tool_def() -> ToolDef {
    ToolDef {
        name: MCP_TOOL_NAME.to_string(),
        description: "Talk to configured MCP servers. Call with \
             {action: 'help'} first to see the available actions \
             (help, list_servers, list_tools, describe_tool, call, status)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["help", "list_servers", "list_tools",
                             "describe_tool", "call", "status"],
                    "description": "Which meta-tool action to invoke"
                },
                "server": {
                    "type": "string",
                    "description": "Server name (required by list_tools / describe_tool / call; optional filter for status)"
                },
                "tool": {
                    "type": "string",
                    "description": "Tool name on the server (required by describe_tool / call)"
                },
                "arguments": {
                    "description": "Tool arguments for call — JSON object, or null/omitted for no-arg tools"
                }
            },
            "required": ["action"]
        }),
    }
}

fn load_config_or_exit() -> McpConfig {
    McpConfig::load().unwrap_or_else(|e| {
        eprintln!("failed to load mcp config: {e:#}");
        std::process::exit(1);
    })
}

/// Construct an `McpRuntimeManager` from on-disk config — returns `None`
/// when no servers are configured so callers can skip the entire MCP path
/// (saves system-prompt tokens + keeps the LLM from hallucinating an empty
/// tool surface). Parse failure falls back to `None` with a `tracing::warn!`.
/// Long-running servers (ACP, future HTTP) call this; CLI subcommands use
/// `load_config_or_exit` instead.
pub fn load_runtime_or_warn() -> Option<McpRuntimeManager> {
    let cfg = McpConfig::load().unwrap_or_else(|e| {
        tracing::warn!("mcp config failed to load, starting with no servers: {e:#}");
        McpConfig::default()
    });
    if cfg.servers.is_empty() {
        None
    } else {
        Some(McpRuntimeManager::from_config(cfg))
    }
}

/// `openab-agent mcp list [--resolve]`.
///
/// Default: print configs verbatim (`${env:VAR}` placeholders kept as-is) so
/// `mcp list` is safe to paste into bug reports. `--resolve` opts into
/// substituting env vars and prints a leading warning — useful for debugging
/// missing-env startup failures locally.
pub fn cli_list_servers(resolve: bool) {
    let cfg = load_config_or_exit();
    if cfg.servers.is_empty() {
        println!("No MCP servers configured.");
        println!("  global:  ~/.openab/agent/mcp.json");
        println!("  project: ./.openab/agent/mcp.json");
        return;
    }
    if resolve {
        eprintln!("⚠ --resolve: env vars substituted into output below.");
        eprintln!("⚠ Output may contain secrets — do not paste publicly.");
        eprintln!();
    }
    let mut servers: Vec<_> = cfg.servers.iter().collect();
    servers.sort_by_key(|(name, _)| *name);
    for (name, server) in servers {
        print_server(name, server, resolve);
    }
}

fn print_server(name: &str, server: &ServerConfig, resolve: bool) {
    if resolve {
        match server.resolved(name) {
            Ok(r) => print_json("✓", name, &r),
            Err(e) => println!("✗ {name}: {e:#}"),
        }
    } else {
        print_json("•", name, server);
    }
}

fn print_json<T: serde::Serialize>(status: &str, name: &str, value: &T) {
    println!("{status} {name}");
    if let Ok(json) = serde_json::to_string_pretty(value) {
        for line in json.lines() {
            println!("    {line}");
        }
    }
}

/// `openab-agent mcp status`.
///
/// Prints per-server runtime status. Servers start `Disconnected` and only
/// advance after `mcp connect <name>` (or, later, lazy dial from the agent
/// path). Servers with an in-flight `mcp-pending:<name>` entry get a
/// `(login pending — run mcp login <name>)` suffix so the user knows the
/// flow stalled mid-paste-back. Orphaned pending entries (no matching
/// config) get listed under a separator so they're visible for cleanup.
pub async fn cli_show_status() {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    if manager.is_empty().await {
        println!("No MCP servers configured.");
        return;
    }
    let statuses = manager.statuses().await;
    let mut pending: std::collections::HashSet<String> =
        manager.pending_logins().into_iter().collect();
    for (name, status) in &statuses {
        let mut line = format!("{} {name}", status.icon());
        if pending.remove(name) {
            line.push_str(&format!(
                " (login pending — run `mcp login {name}` to finish)"
            ));
        } else if matches!(status, runtime::ServerStatus::NeedsAuth) {
            line.push_str(&format!(" (run `mcp login {name}`)"));
        }
        println!("{line}");
    }
    if !pending.is_empty() {
        println!();
        println!("Orphaned pending logins (no matching server in mcp.json):");
        let mut orphans: Vec<String> = pending.into_iter().collect();
        orphans.sort();
        for name in orphans {
            println!("  ⏳ {name}");
        }
    }
}

/// `openab-agent mcp connect <name>`. Spawns the configured stdio server,
/// runs the rmcp handshake, and reports success or the failure reason.
/// The connection is dropped on process exit — this CLI is a smoke-test
/// for `mcp.json` entries, not a long-running session.
pub async fn cli_connect(name: String) {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    match manager.connect(&name).await {
        Ok(()) => println!("● connected: {name}"),
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            std::process::exit(1);
        }
    }
}

/// `openab-agent mcp login <name> [--paste URL]`. Drives the §6.4
/// paste-back flow end-to-end:
///
/// 1. `start_paste_login` builds the authorize URL + pins PKCE state to
///    `auth.json` under `mcp-pending:<name>`
/// 2. The CLI prints the URL for the user to open in a browser, then
///    blocks on stdin waiting for the redirect URL to be pasted back
///    (or skips the prompt when `--paste` was supplied)
/// 3. `complete_login` validates the `state` nonce, exchanges the auth
///    code, persists the resulting `TokenStore`, and clears the pending
///    entry — leaving the server `Disconnected` and ready for `connect`
///
/// Errors at any step exit non-zero; the pending entry is preserved on
/// state-mismatch / network failure so the user can retry with a fresh
/// paste of the same redirect URL without re-running this command.
pub async fn cli_login(name: String, paste: Option<String>) {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    let start = match manager.start_paste_login(&name).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            std::process::exit(1);
        }
    };
    println!("Open this URL in a browser to authorize:");
    println!();
    println!("    {}", start.authorize_url);
    println!();
    println!("State nonce (pinned): {}", start.state);
    println!();
    let redirect = match paste {
        Some(u) => u,
        None => match read_redirect_from_stdin() {
            Ok(u) => u,
            Err(e) => {
                eprintln!("✗ failed to read redirect URL: {e}");
                std::process::exit(1);
            }
        },
    };
    if redirect.is_empty() {
        eprintln!("✗ empty redirect URL — aborting");
        std::process::exit(1);
    }
    match manager.complete_login(&name, &redirect).await {
        Ok(()) => println!("● logged in: {name}"),
        Err(e) => {
            eprintln!("✗ login failed: {e:#}");
            std::process::exit(1);
        }
    }
}

fn read_redirect_from_stdin() -> std::io::Result<String> {
    use std::io::Write;
    print!("Paste the FULL redirect URL: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

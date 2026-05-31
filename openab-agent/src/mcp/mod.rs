//! Native MCP client. See `docs/adr/openab-agent-mcp.md`.

pub mod config;
pub mod runtime;

use config::{McpConfig, ServerConfig};
use runtime::McpRuntimeManager;

fn load_config_or_exit() -> McpConfig {
    McpConfig::load().unwrap_or_else(|e| {
        eprintln!("failed to load mcp config: {e:#}");
        std::process::exit(1);
    })
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
/// path).
pub async fn cli_show_status() {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    if manager.is_empty().await {
        println!("No MCP servers configured.");
        return;
    }
    for (name, status) in manager.statuses().await {
        println!("{} {name}", status.icon());
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

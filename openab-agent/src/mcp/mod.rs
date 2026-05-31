//! Native MCP client. See `docs/adr/openab-agent-mcp.md`.

pub mod config;

use config::McpConfig;

/// `openab-agent mcp list` — load global + project config, resolve env, print.
pub fn cli_list_servers() {
    let cfg = match McpConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load mcp config: {e:#}");
            std::process::exit(1);
        }
    };
    if cfg.servers.is_empty() {
        println!("No MCP servers configured.");
        println!("  global:  ~/.openab/agent/mcp.json");
        println!("  project: ./.openab/agent/mcp.json");
        return;
    }
    let mut servers: Vec<_> = cfg.servers.iter().collect();
    servers.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (name, server) in servers {
        match server.resolved(name) {
            Ok(resolved) => {
                println!("✓ {name}");
                if let Ok(j) = serde_json::to_string_pretty(&resolved) {
                    for line in j.lines() {
                        println!("    {line}");
                    }
                }
            }
            Err(e) => println!("✗ {name}: {e:#}"),
        }
    }
}

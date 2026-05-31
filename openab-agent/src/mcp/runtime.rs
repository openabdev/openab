//! Per-server lifecycle manager. See ADR §5.4 + §5.7.
//!
//! This slice lands only the state-machine scaffold (statuses, handle map,
//! lazy-connect entry point). The actual rmcp `TokioChildProcess` dial +
//! client storage lands in the next slice — keeping that risky bit out of
//! the same commit so any breakage is easy to bisect.

use std::collections::HashMap;

use super::config::{McpConfig, ServerConfig};

/// Per-server status. ADR §5.7: lazy connect — handles start `Disconnected`
/// and transition to `Connecting` only on first use. Connecting / Connected /
/// Failed are wired up by `connect()` in the next slice.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

impl ServerStatus {
    pub fn icon(&self) -> &'static str {
        match self {
            ServerStatus::Disconnected => "○",
            ServerStatus::Connecting => "◐",
            ServerStatus::Connected => "●",
            ServerStatus::Failed(_) => "✗",
        }
    }
}

#[allow(dead_code)] // name + config consumed by connect() in the next slice
#[derive(Debug)]
pub struct ServerHandle {
    pub name: String,
    pub config: ServerConfig,
    pub status: ServerStatus,
}

/// Owns one `ServerHandle` per configured server. Created once at process
/// start (or session start, per ADR §5.8 refresh model).
#[derive(Debug, Default)]
pub struct McpRuntimeManager {
    handles: HashMap<String, ServerHandle>,
}

impl McpRuntimeManager {
    pub fn from_config(cfg: McpConfig) -> Self {
        let handles = cfg
            .servers
            .into_iter()
            .map(|(name, config)| {
                let handle = ServerHandle {
                    name: name.clone(),
                    config,
                    status: ServerStatus::Disconnected,
                };
                (name, handle)
            })
            .collect();
        Self { handles }
    }

    pub fn statuses(&self) -> Vec<(&str, &ServerStatus)> {
        let mut out: Vec<_> = self
            .handles
            .iter()
            .map(|(name, h)| (name.as_str(), &h.status))
            .collect();
        out.sort_by_key(|(name, _)| *name);
        out
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_initializes_each_server_disconnected() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let statuses = mgr.statuses();
        assert_eq!(statuses.len(), 2);
        for (_, status) in statuses {
            assert_eq!(*status, ServerStatus::Disconnected);
        }
    }

    #[test]
    fn empty_config_yields_empty_manager() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        assert!(mgr.is_empty());
        assert!(mgr.statuses().is_empty());
    }

    #[test]
    fn statuses_sorted_by_name() {
        let json = r#"{
            "mcpServers": {
                "zed": { "type": "stdio", "command": "z" },
                "alpha": { "type": "stdio", "command": "a" },
                "mid": { "type": "stdio", "command": "m" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let names: Vec<&str> = mgr.statuses().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha", "mid", "zed"]);
    }
}

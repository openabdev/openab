//! Per-server lifecycle manager. See ADR §5.4 + §5.7.
//!
//! Handles live behind `Arc<tokio::sync::RwLock<...>>` so `connect()` (async,
//! spawns child processes) is `Send` across `.await` and a background idle-
//! eviction task can share the map with foreground `mcp call` invocations
//! (ADR §5.7). Read-heavy / write-light fits `RwLock`.
//!
//! This slice lands the lock migration and a `connect()` that transitions to
//! `Connecting`; the actual rmcp `TokioChildProcess` dial + transition to
//! `Connected` / `Failed` lands in the next slice — keeping that risky bit
//! isolated for bisecting.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::RwLock;

use super::config::{McpConfig, ServerConfig};

#[allow(dead_code)] // Connected / Failed land with the rmcp dial in the next slice
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

#[allow(dead_code)] // name + config consumed by the rmcp dial in the next slice
#[derive(Debug)]
pub struct ServerHandle {
    pub name: String,
    pub config: ServerConfig,
    pub status: ServerStatus,
}

/// Owns one `ServerHandle` per configured server, behind an async `RwLock`
/// so the foreground LLM path and the background eviction task can share it.
#[derive(Debug, Default, Clone)]
pub struct McpRuntimeManager {
    handles: Arc<RwLock<HashMap<String, ServerHandle>>>,
}

impl McpRuntimeManager {
    pub fn from_config(cfg: McpConfig) -> Self {
        let handles: HashMap<_, _> = cfg
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
        Self {
            handles: Arc::new(RwLock::new(handles)),
        }
    }

    /// Snapshot of `(name, status)` sorted by name. Clones out so the read
    /// guard is dropped before returning — callers don't hold a lock.
    pub async fn statuses(&self) -> Vec<(String, ServerStatus)> {
        let mut out: Vec<_> = {
            let guard = self.handles.read().await;
            guard
                .iter()
                .map(|(name, h)| (name.clone(), h.status.clone()))
                .collect()
        };
        out.sort_by(|(a, _), (b, _)| a.cmp(b));
        out
    }

    pub async fn is_empty(&self) -> bool {
        self.handles.read().await.is_empty()
    }

    /// Transition the named server to `Connecting`. The rmcp
    /// `TokioChildProcess` dial + transition to `Connected` / `Failed`
    /// lands in the next slice — see module doc.
    #[allow(dead_code)] // wired into meta-tool dispatch in the next slice; tests keep it covered
    pub async fn connect(&self, name: &str) -> Result<()> {
        let mut guard = self.handles.write().await;
        let handle = guard
            .get_mut(name)
            .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
        handle.status = ServerStatus::Connecting;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn from_config_initializes_each_server_disconnected() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let statuses = mgr.statuses().await;
        assert_eq!(statuses.len(), 2);
        for (_, status) in statuses {
            assert_eq!(status, ServerStatus::Disconnected);
        }
    }

    #[tokio::test]
    async fn empty_config_yields_empty_manager() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        assert!(mgr.is_empty().await);
        assert!(mgr.statuses().await.is_empty());
    }

    #[tokio::test]
    async fn statuses_sorted_by_name() {
        let json = r#"{
            "mcpServers": {
                "zed": { "type": "stdio", "command": "z" },
                "alpha": { "type": "stdio", "command": "a" },
                "mid": { "type": "stdio", "command": "m" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let names: Vec<String> = mgr.statuses().await.into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha", "mid", "zed"]);
    }

    #[tokio::test]
    async fn connect_unknown_server_errors() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        let err = mgr.connect("missing").await.unwrap_err().to_string();
        assert!(err.contains("missing"), "expected 'missing' in {err}");
    }

    #[tokio::test]
    async fn connect_transitions_to_connecting() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "true" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        mgr.connect("fs").await.unwrap();
        let statuses = mgr.statuses().await;
        assert_eq!(statuses[0].1, ServerStatus::Connecting);
    }

    #[tokio::test]
    async fn manager_clone_shares_state() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "true" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let a = McpRuntimeManager::from_config(cfg);
        let b = a.clone();
        a.connect("fs").await.unwrap();
        assert_eq!(b.statuses().await[0].1, ServerStatus::Connecting);
    }
}

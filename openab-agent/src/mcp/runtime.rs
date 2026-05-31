//! Per-server lifecycle manager. See ADR §5.4 + §5.7.
//!
//! Handles live behind `Arc<tokio::sync::RwLock<...>>` so `connect()` (async,
//! spawns child processes) is `Send` across `.await` and a background idle-
//! eviction task can share the map with foreground `mcp call` invocations
//! (ADR §5.7). Read-heavy / write-light fits `RwLock`.
//!
//! `connect()` uses a double-lock pattern: a short write lock to mark
//! `Connecting`, release the lock, run the rmcp handshake without holding
//! any lock, then re-acquire briefly to install the client or record the
//! failure. Holding the write lock across the `serve(...).await` would
//! starve every reader (including `mcp status` and the eviction scan) for
//! the duration of a child-process spawn + handshake.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use tokio::process::Command;
use tokio::sync::RwLock;

use super::config::{McpConfig, ServerConfig};

#[allow(dead_code)] // NeedsAuth lands with the Phase 2 OAuth slice (ADR §5.7)
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

pub struct ServerHandle {
    pub name: String,
    pub config: ServerConfig,
    pub status: ServerStatus,
    pub client: Option<RunningService<RoleClient, ()>>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("status", &self.status)
            .field("client", &self.client.is_some())
            .finish()
    }
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
                    client: None,
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

    /// Lazy-connect the named server (ADR §5.7). Idempotent if already
    /// `Connected` with a live client. HTTP transport is Phase 2.
    pub async fn connect(&self, name: &str) -> Result<()> {
        let dial = {
            let mut guard = self.handles.write().await;
            let handle = guard
                .get_mut(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            if matches!(handle.status, ServerStatus::Connected) && handle.client.is_some() {
                return Ok(());
            }
            let resolved = handle.config.resolved(name)?;
            let dial = match resolved {
                ServerConfig::Stdio {
                    command, args, env, ..
                } => StdioDial { command, args, env },
                ServerConfig::Http { .. } => {
                    return Err(anyhow!("http transport lands in phase 2 (server {name:?})"));
                }
            };
            handle.status = ServerStatus::Connecting;
            dial
        };

        let dial_result = dial.run().await;

        let mut guard = self.handles.write().await;
        let handle = guard
            .get_mut(name)
            .ok_or_else(|| anyhow!("server {name:?} vanished during connect"))?;
        // Race guard: a concurrent connect() may have installed a client while
        // we were dialing. Yield to the winner — `dial_result` drops here,
        // killing the duplicate child via RunningService's Drop impl.
        if matches!(handle.status, ServerStatus::Connected) && handle.client.is_some() {
            return Ok(());
        }
        match dial_result {
            Ok(client) => {
                handle.status = ServerStatus::Connected;
                handle.client = Some(client);
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                handle.status = ServerStatus::Failed(msg.clone());
                Err(anyhow!(msg))
            }
        }
    }
}

struct StdioDial {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
}

impl StdioDial {
    async fn run(self) -> Result<RunningService<RoleClient, ()>> {
        let Self { command, args, env } = self;
        let cmd = Command::new(&command).configure(|c| {
            c.args(&args);
            c.envs(&env);
        });
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("spawn mcp child process {command:?}"))?;
        ().serve(transport)
            .await
            .with_context(|| format!("mcp handshake with {command:?}"))
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
    async fn connect_http_returns_phase2_error() {
        let json = r#"{
            "mcpServers": {
                "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = mgr.connect("linear").await.unwrap_err().to_string();
        assert!(err.contains("phase 2"), "expected 'phase 2' in {err}");
        // Status not advanced past Disconnected for unsupported transports.
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::Disconnected);
    }

    #[tokio::test]
    async fn connect_to_missing_binary_records_failed() {
        let json = r#"{
            "mcpServers": {
                "broken": {
                    "type": "stdio",
                    "command": "/nonexistent/path/openab-mcp-test-stub-zzz"
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = mgr.connect("broken").await.unwrap_err().to_string();
        assert!(err.contains("spawn"), "expected 'spawn' in {err}");
        match &mgr.statuses().await[0].1 {
            ServerStatus::Failed(msg) => assert!(msg.contains("spawn")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy)]
pub enum PoolProgress {
    Spawning,
    Initializing,
    CreatingSession,
}

pub struct SessionPool {
    connections: RwLock<HashMap<String, AcpConnection>>,
    config: AgentConfig,
    max_sessions: usize,
}

impl SessionPool {
    pub fn new(config: AgentConfig, max_sessions: usize) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            config,
            max_sessions,
        }
    }

    pub async fn get_or_create<F, Fut>(&self, thread_id: &str, on_progress: F) -> Result<()>
    where
        F: Fn(PoolProgress) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Check if alive connection exists
        {
            let conns = self.connections.read().await;
            if let Some(conn) = conns.get(thread_id) {
                if conn.alive() {
                    return Ok(());
                }
            }
        }

        // Brief write lock: validate capacity and clean stale entry
        let is_rebuild = {
            let mut conns = self.connections.write().await;
            let rebuilding = if let Some(conn) = conns.get(thread_id) {
                if conn.alive() {
                    return Ok(());
                }
                warn!(thread_id, "stale connection, rebuilding");
                conns.remove(thread_id);
                true
            } else {
                false
            };
            if conns.len() >= self.max_sessions {
                return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
            }
            rebuilding
        }; // write lock dropped here

        // Spawn and initialize outside of lock
        on_progress(PoolProgress::Spawning).await;

        let mut conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &self.config.working_dir,
            &self.config.env,
        )
        .await?;

        on_progress(PoolProgress::Initializing).await;

        conn.initialize().await?;

        on_progress(PoolProgress::CreatingSession).await;

        conn.session_new(&self.config.working_dir).await?;

        if is_rebuild {
            conn.session_reset = true;
        }

        // Re-acquire write lock only to insert
        let mut conns = self.connections.write().await;
        conns.insert(thread_id.to_string(), conn);
        Ok(())
    }

    /// Get mutable access to a connection. Caller must have called get_or_create first.
    pub async fn with_connection<F, R>(&self, thread_id: &str, f: F) -> Result<R>
    where
        F: FnOnce(&mut AcpConnection) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<R>> + Send + '_>>,
    {
        let mut conns = self.connections.write().await;
        let conn = conns
            .get_mut(thread_id)
            .ok_or_else(|| anyhow!("no connection for thread {thread_id}"))?;
        f(conn).await
    }

    pub async fn cleanup_idle(&self, ttl_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(ttl_secs);
        let mut conns = self.connections.write().await;
        let stale: Vec<String> = conns
            .iter()
            .filter(|(_, c)| c.last_active < cutoff || !c.alive())
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale {
            info!(thread_id = %key, "cleaning up idle session");
            conns.remove(&key);
            // Child process killed via kill_on_drop when AcpConnection drops
        }
    }

    pub async fn shutdown(&self) {
        let mut conns = self.connections.write().await;
        let count = conns.len();
        conns.clear(); // kill_on_drop handles process cleanup
        info!(count, "pool shutdown complete");
    }
}

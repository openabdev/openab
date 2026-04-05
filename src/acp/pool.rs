use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{info, warn};

pub struct SessionPool {
    connections: RwLock<HashMap<String, Arc<Mutex<AcpConnection>>>>,
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

    pub async fn get_or_create(&self, thread_id: &str) -> Result<()> {
        // Check if alive connection exists (read lock only)
        {
            let conns = self.connections.read().await;
            if let Some(conn_arc) = conns.get(thread_id) {
                let conn = conn_arc.lock().await;
                if conn.alive() {
                    return Ok(());
                }
            }
        }

        // Need to create or rebuild (write lock)
        let mut conns = self.connections.write().await;

        // Double-check after acquiring write lock
        if let Some(conn_arc) = conns.get(thread_id) {
            let conn = conn_arc.lock().await;
            if conn.alive() {
                return Ok(());
            }
            drop(conn);
            warn!(thread_id, "stale connection, rebuilding");
            conns.remove(thread_id);
        }

        if conns.len() >= self.max_sessions {
            return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
        }

        let mut conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &self.config.working_dir,
            &self.config.env,
        )
        .await?;

        conn.initialize().await?;
        conn.session_new(&self.config.working_dir).await?;

        let is_rebuild = conns.contains_key(thread_id);
        if is_rebuild {
            conn.session_reset = true;
        }

        conns.insert(thread_id.to_string(), Arc::new(Mutex::new(conn)));
        Ok(())
    }

    /// Get a shared reference to a connection's lock.
    /// The pool RwLock is only held briefly for the lookup (read lock).
    /// The caller then locks only their own connection — other sessions
    /// remain fully accessible.
    pub async fn get_connection(&self, thread_id: &str) -> Result<Arc<Mutex<AcpConnection>>> {
        let conns = self.connections.read().await;
        conns
            .get(thread_id)
            .cloned()
            .ok_or_else(|| anyhow!("no connection for thread {thread_id}"))
    }

    pub async fn cleanup_idle(&self, ttl_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(ttl_secs);
        let mut conns = self.connections.write().await;
        let mut stale = Vec::new();
        for (key, conn_arc) in conns.iter() {
            let conn = conn_arc.lock().await;
            if conn.last_active < cutoff || !conn.alive() {
                stale.push(key.clone());
            }
        }
        for key in stale {
            info!(thread_id = %key, "cleaning up idle session");
            conns.remove(&key);
        }
    }

    pub async fn shutdown(&self) {
        let mut conns = self.connections.write().await;
        let count = conns.len();
        conns.clear();
        info!(count, "pool shutdown complete");
    }
}

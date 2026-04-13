use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{info, warn};

pub struct SessionPool {
    connections: RwLock<HashMap<String, AcpConnection>>,
    /// Suspended sessions: thread_key → ACP sessionId.
    /// When a connection is evicted, its sessionId is saved here so it can be
    /// resumed via `session/load` when the user returns.
    suspended: RwLock<HashMap<String, String>>,
    config: AgentConfig,
    max_sessions: usize,
}

impl SessionPool {
    pub fn new(config: AgentConfig, max_sessions: usize) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            suspended: RwLock::new(HashMap::new()),
            config,
            max_sessions,
        }
    }

    pub async fn get_or_create(&self, thread_id: &str) -> Result<()> {
        // Check if alive connection exists
        {
            let conns = self.connections.read().await;
            if let Some(conn) = conns.get(thread_id) {
                if conn.alive() {
                    return Ok(());
                }
            }
        }

        // Need to create or rebuild
        let mut conns = self.connections.write().await;

        // Double-check after acquiring write lock
        if let Some(conn) = conns.get(thread_id) {
            if conn.alive() {
                return Ok(());
            }
            warn!(thread_id, "stale connection, rebuilding");
            self.suspend_locked(&mut conns, thread_id).await;
        }

        if conns.len() >= self.max_sessions {
            // LRU evict: suspend the oldest idle session to make room
            let oldest = conns
                .iter()
                .min_by_key(|(_, c)| c.last_active)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest {
                info!(evicted = %key, "pool full, suspending oldest idle session");
                self.suspend_locked(&mut conns, &key).await;
            } else {
                return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
            }
        }

        let mut conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &self.config.working_dir,
            &self.config.env,
        )
        .await?;

        conn.initialize().await?;

        // Try to resume a suspended session via session/load
        let saved_session_id = self.suspended.write().await.remove(thread_id);
        let mut resumed = false;
        if let Some(ref sid) = saved_session_id {
            if conn.supports_load_session {
                match conn.session_load(sid, &self.config.working_dir).await {
                    Ok(()) => {
                        info!(thread_id, session_id = %sid, "session resumed via session/load");
                        resumed = true;
                    }
                    Err(e) => {
                        warn!(thread_id, session_id = %sid, error = %e, "session/load failed, creating new session");
                    }
                }
            }
        }

        if !resumed {
            conn.session_new(&self.config.working_dir).await?;
            if saved_session_id.is_some() {
                // Had a suspended session but couldn't resume — mark as reset
                conn.session_reset = true;
            }
        }

        conns.insert(thread_id.to_string(), conn);
        Ok(())
    }

    /// Suspend a connection: save its sessionId and remove from active map.
    /// Must be called with write lock held on `connections`.
    async fn suspend_locked(
        &self,
        conns: &mut HashMap<String, AcpConnection>,
        thread_id: &str,
    ) {
        if let Some(conn) = conns.remove(thread_id) {
            if let Some(sid) = &conn.acp_session_id {
                info!(thread_id, session_id = %sid, "suspending session");
                self.suspended.write().await.insert(thread_id.to_string(), sid.clone());
            }
            // conn dropped here → Drop impl kills process group
        }
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
            self.suspend_locked(&mut conns, &key).await;
        }
    }

    pub async fn shutdown(&self) {
        let mut conns = self.connections.write().await;
        let count = conns.len();
        conns.clear(); // Drop impl kills process groups
        info!(count, "pool shutdown complete");
    }
}

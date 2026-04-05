use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{info, warn};

/// Minimal chat context needed to notify the user when a session is evicted.
#[derive(Clone)]
pub struct SessionMeta {
    pub chat_id: i64,
    pub thread_id: Option<i32>,
}

/// Callback invoked by cleanup_idle for each evicted session.
pub type EvictNotifier = Arc<dyn Fn(SessionMeta) + Send + Sync>;

use std::sync::Arc;

pub struct SessionPool {
    connections: RwLock<HashMap<String, AcpConnection>>,
    meta: RwLock<HashMap<String, SessionMeta>>,
    config: AgentConfig,
    max_sessions: usize,
    pub evict_notifier: Mutex<Option<EvictNotifier>>,
}

impl SessionPool {
    pub fn new(config: AgentConfig, max_sessions: usize) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            meta: RwLock::new(HashMap::new()),
            config,
            max_sessions,
            evict_notifier: Mutex::new(None),
        }
    }

    pub fn with_evict_notifier(self, f: EvictNotifier) -> Self {
        *self.evict_notifier.lock().unwrap() = Some(f);
        self
    }

    /// Store chat context for a session so cleanup can notify the user.
    pub async fn register_meta(&self, session_key: &str, meta: SessionMeta) {
        self.meta.write().await.insert(session_key.to_string(), meta);
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
            conns.remove(thread_id);
        }

        if conns.len() >= self.max_sessions {
            return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
        }

        // Sanitize session key into a safe directory name
        let safe_key = thread_id.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
        let session_dir = format!("{}/session-{}", self.config.working_dir, safe_key);
        tokio::fs::create_dir_all(&session_dir).await
            .map_err(|e| anyhow!("failed to create session dir {session_dir}: {e}"))?;

        let mut conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &session_dir,
            &self.config.env,
        )
        .await?;

        conn.initialize().await?;
        conn.session_new(&session_dir).await?;

        let is_rebuild = conns.contains_key(thread_id);
        if is_rebuild {
            conn.session_reset = true;
        }

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
        let stale: Vec<(String, Option<SessionMeta>)> = {
            let conns = self.connections.read().await;
            let meta = self.meta.read().await;
            conns
                .iter()
                .filter(|(_, c)| !c.is_streaming && (c.last_active < cutoff || !c.alive()))
                .map(|(k, _)| (k.clone(), meta.get(k).cloned()))
                .collect()
        };
        if stale.is_empty() { return; }
        let mut conns = self.connections.write().await;
        let mut meta = self.meta.write().await;
        for (key, session_meta) in stale {
            info!(thread_id = %key, "cleaning up idle session");
            conns.remove(&key);
            meta.remove(&key);
            if let (Some(notifier), Some(m)) = (self.evict_notifier.lock().unwrap().as_ref().cloned(), session_meta) {
                notifier(m);
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut conns = self.connections.write().await;
        let count = conns.len();
        conns.clear();
        self.meta.write().await.clear();
        info!(count, "pool shutdown complete");
    }
}

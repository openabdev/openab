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
    /// Persists the last ACP session ID for each thread key across evictions.
    prev_session_ids: RwLock<HashMap<String, String>>,
    config: AgentConfig,
    max_sessions: usize,
    pub evict_notifier: Mutex<Option<EvictNotifier>>,
}

impl SessionPool {
    pub fn new(config: AgentConfig, max_sessions: usize) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            meta: RwLock::new(HashMap::new()),
            prev_session_ids: RwLock::new(HashMap::new()),
            config,
            max_sessions,
            evict_notifier: Mutex::new(None),
        }
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

        // Look up any previously evicted session ID for this thread.
        let prev_sid = self.prev_session_ids.read().await.get(thread_id).cloned();

        let resumed = if let Some(ref sid) = prev_sid {
            if conn.supports_load_session {
                match conn.session_load(&session_dir, sid).await {
                    Ok(_) => {
                        info!(thread_id, session_id = %sid, "true resume via session/load");
                        self.prev_session_ids.write().await.remove(thread_id);
                        true
                    }
                    Err(e) => {
                        warn!(thread_id, "session/load failed ({e}), falling back to session/new");
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        if !resumed {
            conn.session_new(&session_dir).await?;

            // Fallback: if there was a previous session, inject history as context.
            if let Some(ref sid) = prev_sid {
                if let Some(history) = load_history_summary(&session_dir, sid).await {
                    // Mark for the caller that this is a resumed-with-history session.
                    conn.session_reset = false; // not a cold reset
                    // Inject history as a silent first prompt so the agent has context.
                    let (mut rx, _) = conn.session_prompt(&history).await?;
                    while let Some(msg) = rx.recv().await {
                        if msg.id.is_some() { break; }
                    }
                    conn.prompt_done().await;
                    info!(thread_id, "history injected via fallback prompt");
                    self.prev_session_ids.write().await.remove(thread_id);
                } else {
                    conn.session_reset = true;
                }
            }
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
        let stale: Vec<(String, Option<SessionMeta>, Option<String>)> = {
            let conns = self.connections.read().await;
            let meta = self.meta.read().await;
            conns
                .iter()
                .filter(|(_, c)| !c.is_streaming && (c.last_active < cutoff || !c.alive()))
                .map(|(k, c)| (k.clone(), meta.get(k).cloned(), c.acp_session_id.clone()))
                .collect()
        };
        if stale.is_empty() { return; }
        let mut conns = self.connections.write().await;
        let mut meta = self.meta.write().await;
        let mut prev = self.prev_session_ids.write().await;
        for (key, session_meta, acp_session_id) in stale {
            info!(thread_id = %key, "cleaning up idle session");
            conns.remove(&key);
            meta.remove(&key);
            if let Some(sid) = acp_session_id {
                prev.insert(key.clone(), sid);
            }
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

/// Read the last N turns from kiro's session JSONL and return a compact history prompt.
/// Returns None if the file doesn't exist or has no usable content.
async fn load_history_summary(_session_dir: &str, session_id: &str) -> Option<String> {
    // Kiro stores sessions in ~/.kiro/sessions/cli/<session_id>.jsonl
    let home = std::env::var("HOME").ok()?;
    let path = format!("{home}/.kiro/sessions/cli/{session_id}.jsonl");
    let content = tokio::fs::read_to_string(&path).await.ok()?;

    let mut turns: Vec<(String, String)> = Vec::new(); // (role, text)
    for line in content.lines() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let kind = v.get("kind")?.as_str()?;
        let role = match kind {
            "Prompt" => "user",
            "AssistantMessage" => "assistant",
            _ => continue,
        };
        let text = v.get("data")
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("data"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !text.is_empty() {
            turns.push((role.to_string(), text));
        }
    }

    if turns.is_empty() {
        return None;
    }

    // Keep last 10 turns to stay within context limits
    let recent = if turns.len() > 10 { &turns[turns.len() - 10..] } else { &turns[..] };
    let mut summary = String::from(
        "[Resuming previous session. Conversation history for context:]\n"
    );
    for (role, text) in recent {
        let preview: String = text.chars().take(500).collect();
        summary.push_str(&format!("{}: {}\n", role, preview));
    }
    summary.push_str("[End of history. Continue from here.]\n");
    Some(summary)
}

use crate::acp::connection::AcpConnection;
use crate::acp::protocol::ConfigOption;
use crate::adapter::{ChannelRef, ChatAdapter};
use crate::config::AgentConfig;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{info, warn};

/// A single active session: connection handle + addressing info bundled
/// together so their lifetimes are enforced at the type level.
struct SessionEntry {
    conn: Arc<Mutex<AcpConnection>>,
    channel: ChannelRef,
    adapter: Arc<dyn ChatAdapter>,
}

/// Combined state protected by a single lock to prevent deadlocks.
/// Lock ordering: never await a per-connection mutex while holding `state`.
struct PoolState {
    /// Active sessions: thread_key → SessionEntry (connection + addressing).
    sessions: HashMap<String, SessionEntry>,
    /// Lock-free cancel handles: thread_key → (stdin, session_id).
    /// Stored separately so cancel can work without locking the connection.
    cancel_handles: HashMap<String, (Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>, String)>,
    /// Suspended sessions: thread_key → ACP sessionId.
    /// Saved on eviction so sessions can be resumed via `session/load`.
    suspended: HashMap<String, String>,
    /// Serializes create/resume work per thread so rapid same-thread requests
    /// cannot race each other into duplicate `session/load` attempts.
    creating: HashMap<String, Arc<Mutex<()>>>,
}

pub struct SessionPool {
    state: RwLock<PoolState>,
    config: AgentConfig,
    max_sessions: usize,
    /// Flipped by `begin_shutdown` to reject new admissions. Checked inside
    /// `get_or_create` under the state write lock so admission and snapshot
    /// are atomic.
    shutting_down: std::sync::atomic::AtomicBool,
}

type EvictionCandidate = (
    String,
    Arc<Mutex<AcpConnection>>,
    Instant,
    Option<String>,
);

/// Remove a session entry only if its connection handle matches `expected`.
/// Returns the removed connection Arc, or None if the handle was swapped.
fn remove_if_same_conn(
    map: &mut HashMap<String, SessionEntry>,
    key: &str,
    expected: &Arc<Mutex<AcpConnection>>,
) -> Option<Arc<Mutex<AcpConnection>>> {
    let should_remove = map
        .get(key)
        .is_some_and(|entry| Arc::ptr_eq(&entry.conn, expected));
    if should_remove {
        map.remove(key).map(|e| e.conn)
    } else {
        None
    }
}

fn get_or_insert_gate(
    map: &mut HashMap<String, Arc<Mutex<()>>>,
    key: &str,
) -> Arc<Mutex<()>> {
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

impl SessionPool {
    pub fn new(config: AgentConfig, max_sessions: usize) -> Self {
        Self {
            state: RwLock::new(PoolState {
                sessions: HashMap::new(),
                cancel_handles: HashMap::new(),
                suspended: HashMap::new(),
                creating: HashMap::new(),
            }),
            config,
            max_sessions,
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// True once `begin_shutdown` has been called. Router uses this to show a
    /// shutdown-specific message instead of a generic pool error when
    /// `get_or_create` rejects admission.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Flip the pool into shutting-down state and return a snapshot of every
    /// live session's addressing info. Takes the state write lock so the
    /// snapshot is atomic with respect to in-flight `get_or_create` calls:
    /// any admission that committed before us is included; any that comes
    /// after us sees the flag inside the same lock and rejects.
    pub async fn begin_shutdown(&self) -> Vec<(String, ChannelRef, Arc<dyn ChatAdapter>)> {
        let state = self.state.write().await;
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        state
            .sessions
            .iter()
            .map(|(k, e)| (k.clone(), e.channel.clone(), e.adapter.clone()))
            .collect()
    }

    pub async fn get_or_create(
        &self,
        thread_id: &str,
        channel: &ChannelRef,
        adapter: &Arc<dyn ChatAdapter>,
    ) -> Result<()> {
        // Fast-fail: avoid spawning a fresh ACP process if shutdown is already
        // in progress. The authoritative check happens again under the state
        // write lock below so we also catch shutdowns that start mid-spawn.
        if self.is_shutting_down() {
            bail!("pool is shutting down");
        }

        let create_gate = {
            let mut state = self.state.write().await;
            get_or_insert_gate(&mut state.creating, thread_id)
        };
        let _create_guard = create_gate.lock().await;

        let (existing, saved_session_id) = {
            let state = self.state.read().await;
            if self.is_shutting_down() {
                bail!("pool is shutting down");
            }
            (
                state.sessions.get(thread_id).map(|e| e.conn.clone()),
                state.suspended.get(thread_id).cloned(),
            )
        };

        let had_existing = existing.is_some();
        let mut saved_session_id = saved_session_id;
        if let Some(conn) = existing.clone() {
            let conn = conn.lock().await;
            if conn.alive() {
                // Re-check shutdown state after waiting on the per-connection
                // mutex. Taking `state.read()` synchronizes us with
                // `begin_shutdown`'s write-lock flag flip, so the flag value
                // we see here reflects every `begin_shutdown` that has
                // committed. This closes the race where shutdown starts
                // while we were waiting on `conn.lock()`.
                //
                // DO NOT REMOVE — this read-lock is a synchronization barrier,
                // not a data access. Without it the `is_shutting_down()` check
                // below can observe a stale value.
                let _shutdown_barrier = self.state.read().await;
                if self.is_shutting_down() {
                    bail!("pool is shutting down");
                }
                return Ok(());
            }
            if saved_session_id.is_none() {
                saved_session_id = conn.acp_session_id.clone();
            }
        }

        // Snapshot active handles so we can inspect them outside the state lock.
        let snapshot: Vec<(String, Arc<Mutex<AcpConnection>>)> = {
            let state = self.state.read().await;
            state
                .sessions
                .iter()
                .map(|(k, e)| (k.clone(), Arc::clone(&e.conn)))
                .collect()
        };

        let mut eviction_candidate: Option<EvictionCandidate> = None;
        let mut skipped_locked_candidates = 0usize;
        for (key, conn) in snapshot {
            if key == thread_id {
                continue;
            }
            let conn_handle = Arc::clone(&conn);
            let Ok(conn) = conn.try_lock() else {
                skipped_locked_candidates += 1;
                continue;
            };
            let candidate = (key, conn_handle, conn.last_active, conn.acp_session_id.clone());
            match &eviction_candidate {
                Some((_, _, oldest_last_active, _)) if candidate.2 >= *oldest_last_active => {}
                _ => eviction_candidate = Some(candidate),
            }
        }

        // Build the replacement connection outside the state lock so one stuck
        // initialization does not block all unrelated sessions.
        let mut new_conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &self.config.working_dir,
            &self.config.env,
        )
        .await?;

        new_conn.initialize().await?;

        let mut resumed = false;
        if let Some(ref sid) = saved_session_id {
            if new_conn.supports_load_session {
                match new_conn.session_load(sid, &self.config.working_dir).await {
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
            new_conn.session_new(&self.config.working_dir).await?;
            // Surface the reset banner both for restored sessions and for stale
            // live entries that died before we could recover a resumable
            // session id. In both cases the caller is continuing after an
            // unexpected session loss.
            if had_existing || saved_session_id.is_some() {
                new_conn.session_reset = true;
            }
        }

        let cancel_handle = new_conn.cancel_handle();
        let cancel_session_id = new_conn.acp_session_id.clone().unwrap_or_default();
        let new_conn = Arc::new(Mutex::new(new_conn));

        let mut state = self.state.write().await;

        // Admission check inside the state write lock. This is atomic with
        // `begin_shutdown`'s flag-flip + snapshot: a shutdown that started
        // during our ACP spawn is caught here, and our work is thrown away
        // rather than being added to a pool that is about to be torn down.
        if self.is_shutting_down() {
            bail!("pool is shutting down");
        }

        // Another task may have created a healthy connection while we were
        // initializing this one.
        if let Some(entry) = state.sessions.get(thread_id) {
            let existing = entry.conn.clone();
            let Ok(existing) = existing.try_lock() else {
                return Ok(());
            };
            if existing.alive() {
                return Ok(());
            }
            warn!(thread_id, "stale connection, rebuilding");
            drop(existing);
            state.sessions.remove(thread_id);
        }

        if state.sessions.len() >= self.max_sessions {
            if let Some((key, expected_conn, _, sid)) = eviction_candidate {
                if remove_if_same_conn(&mut state.sessions, &key, &expected_conn).is_some() {
                    info!(evicted = %key, "pool full, suspending oldest idle session");
                    if let Some(sid) = sid {
                        state.suspended.insert(key, sid);
                    }
                } else {
                    warn!(evicted = %key, "pool full but eviction candidate changed before removal");
                }
            } else if skipped_locked_candidates > 0 {
                warn!(
                    max_sessions = self.max_sessions,
                    skipped_locked_candidates,
                    "pool full but all other sessions were busy during eviction scan"
                );
            }
        }

        if state.sessions.len() >= self.max_sessions {
            return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
        }

        state.suspended.remove(thread_id);
        state.sessions.insert(thread_id.to_string(), SessionEntry {
            conn: new_conn,
            channel: channel.clone(),
            adapter: adapter.clone(),
        });
        if !cancel_session_id.is_empty() {
            state.cancel_handles.insert(thread_id.to_string(), (cancel_handle, cancel_session_id));
        }
        Ok(())
    }

    /// Get mutable access to a connection. Caller must have called get_or_create first.
    pub async fn with_connection<F, R>(&self, thread_id: &str, f: F) -> Result<R>
    where
        F: for<'a> FnOnce(
            &'a mut AcpConnection,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>>,
    {
        let conn = {
            let state = self.state.read().await;
            state
                .sessions
                .get(thread_id)
                .map(|e| e.conn.clone())
                .ok_or_else(|| anyhow!("no connection for thread {thread_id}"))?
        };

        let mut conn = conn.lock().await;
        f(&mut conn).await
    }

    /// Get cached configOptions for a session (e.g. available models).
    pub async fn get_config_options(&self, thread_id: &str) -> Vec<ConfigOption> {
        let state = self.state.read().await;
        let conn = match state.sessions.get(thread_id) {
            Some(e) => e.conn.clone(),
            None => return Vec::new(),
        };
        drop(state);
        let conn = conn.lock().await;
        conn.config_options.clone()
    }

    /// Set a config option (e.g. model) via ACP and return updated options.
    pub async fn set_config_option(
        &self,
        thread_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<ConfigOption>> {
        let conn = {
            let state = self.state.read().await;
            state
                .sessions
                .get(thread_id)
                .map(|e| e.conn.clone())
                .ok_or_else(|| anyhow!("no connection for thread {thread_id}"))?
        };
        let mut conn = conn.lock().await;
        conn.set_config_option(config_id, value).await
    }

    /// Cancel the current in-flight operation for a session.
    /// Uses pre-stored cancel handles to avoid locking the connection (which is held during streaming).
    pub async fn cancel_session(&self, thread_id: &str) -> Result<()> {
        let (stdin, session_id) = {
            let state = self.state.read().await;
            state.cancel_handles.get(thread_id).cloned()
                .ok_or_else(|| anyhow!("no session for thread {thread_id}"))?
        };
        let data = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }))?;
        tracing::info!(session_id, "sending session/cancel");
        use tokio::io::AsyncWriteExt;
        let mut w = stdin.lock().await;
        w.write_all(data.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn cleanup_idle(&self, ttl_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(ttl_secs);

        let snapshot: Vec<(String, Arc<Mutex<AcpConnection>>)> = {
            let state = self.state.read().await;
            state
                .sessions
                .iter()
                .map(|(k, e)| (k.clone(), Arc::clone(&e.conn)))
                .collect()
        };

        let mut stale = Vec::new();
        for (key, conn) in snapshot {
            // Skip active sessions for this cleanup round instead of waiting on
            // their per-connection mutex. A busy session is not idle.
            let conn_handle = Arc::clone(&conn);
            let Ok(conn) = conn.try_lock() else {
                continue;
            };
            if conn.last_active < cutoff || !conn.alive() {
                stale.push((key, conn_handle, conn.acp_session_id.clone()));
            }
        }

        if stale.is_empty() {
            return;
        }

        let mut state = self.state.write().await;
        for (key, expected_conn, sid) in stale {
            if remove_if_same_conn(&mut state.sessions, &key, &expected_conn).is_some() {
                info!(thread_id = %key, "cleaning up idle session");
                if let Some(sid) = sid {
                    state.suspended.insert(key, sid);
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.write().await;
        let count = state.sessions.len();
        state.sessions.clear(); // Drop impl kills process groups
        info!(count, "pool shutdown complete");
    }
}

#[cfg(test)]
mod tests {
    use super::get_or_insert_gate;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// The pointer-equality removal logic used by `remove_if_same_conn`.
    /// Tested here with a simple type since `AcpConnection` cannot be
    /// constructed in unit tests.
    fn remove_if_same_handle<T>(
        map: &mut HashMap<String, Arc<Mutex<T>>>,
        key: &str,
        expected: &Arc<Mutex<T>>,
    ) -> Option<Arc<Mutex<T>>> {
        let should_remove = map
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, expected));
        if should_remove {
            map.remove(key)
        } else {
            None
        }
    }

    #[test]
    fn remove_if_same_handle_removes_matching_entry() {
        let expected = Arc::new(Mutex::new(1_u8));
        let mut map = HashMap::from([("thread".to_string(), Arc::clone(&expected))]);

        let removed = remove_if_same_handle(&mut map, "thread", &expected);

        assert!(removed.is_some());
        assert!(map.is_empty());
    }

    #[test]
    fn remove_if_same_handle_keeps_replaced_entry() {
        let stale = Arc::new(Mutex::new(1_u8));
        let fresh = Arc::new(Mutex::new(2_u8));
        let mut map = HashMap::from([("thread".to_string(), Arc::clone(&fresh))]);

        let removed = remove_if_same_handle(&mut map, "thread", &stale);

        assert!(removed.is_none());
        let current = map.get("thread").expect("entry should remain");
        assert!(Arc::ptr_eq(current, &fresh));
    }

    #[test]
    fn get_or_insert_gate_reuses_gate_for_same_thread() {
        let mut map = HashMap::new();

        let first = get_or_insert_gate(&mut map, "thread");
        let second = get_or_insert_gate(&mut map, "thread");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(map.len(), 1);
    }
}

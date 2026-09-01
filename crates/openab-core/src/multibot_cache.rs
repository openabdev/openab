//! Persistent disk cache for multibot thread detection.
//!
//! Once a thread is identified as multi-bot (irreversible), it is stored in
//! `~/.openab/cache/threads.json` so the detection survives restarts and
//! in-memory TTL expiry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    detected_at: DateTime<Utc>,
}

/// Shared multibot thread cache with file persistence.
#[derive(Clone)]
pub struct MultibotCache {
    threads: Arc<Mutex<HashMap<String, Entry>>>,
    path: PathBuf,
}

impl MultibotCache {
    /// Load or create the cache from `~/.openab/cache/threads.json`.
    pub fn load(path: PathBuf) -> Self {
        let threads = match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                warn!(error = %e, "failed to parse threads.json, starting empty");
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        info!(count = threads.len(), path = %path.display(), "loaded multibot cache");
        Self {
            threads: Arc::new(Mutex::new(threads)),
            path,
        }
    }

    /// Check if a thread is known to be multi-bot.
    pub fn is_multibot(&self, thread_id: &str) -> bool {
        self.threads.lock().unwrap().contains_key(thread_id)
    }

    /// Mark a thread as multi-bot and persist to disk (non-blocking).
    pub async fn mark_multibot(&self, thread_id: &str) {
        let snapshot = {
            let mut threads = self.threads.lock().unwrap();
            if threads.contains_key(thread_id) {
                return;
            }
            threads.insert(
                thread_id.to_string(),
                Entry {
                    detected_at: Utc::now(),
                },
            );
            threads.clone()
        };
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || persist(&path, &snapshot))
            .await
            .ok();
    }
}

fn persist(path: &PathBuf, threads: &HashMap<String, Entry>) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, "failed to create cache directory");
            return;
        }
    }
    match serde_json::to_string_pretty(threads) {
        Ok(data) => {
            if let Err(e) = std::fs::write(path, data) {
                warn!(error = %e, "failed to persist threads.json");
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to serialize multibot cache");
        }
    }
}

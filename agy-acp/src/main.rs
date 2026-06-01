use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Option<u64>,
    method: Option<String>,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: Value,
}

/// Persisted session→conversation mapping stored in ~/.openab/agy-acp/sessions.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionStore {
    sessions: HashMap<String, StoredSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    conversation_id: Option<String>,
    #[serde(default)]
    emitted_line_count: usize,
}

struct Session {
    conversation_id: Option<String>,
    /// Number of already-emitted stdout lines for the current conversation.
    emitted_line_count: usize,
}

struct Adapter {
    sessions: HashMap<String, Session>,
    working_dir: String,
    conversations_dir: PathBuf,
    state_file: PathBuf,
}

impl Adapter {
    fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let state_dir = PathBuf::from(&home).join(".openab/agy-acp");
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
            conversations_dir: PathBuf::from(&home).join(".gemini/antigravity-cli/conversations"),
            state_file: state_dir.join("sessions.json"),
        }
    }

    /// Acquire exclusive lock on a dedicated lock file for read-write mutual exclusion.
    fn lock_state_file(&self) -> Option<fs::File> {
        if let Some(parent) = self.state_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let lock_path = self.state_file.with_extension("lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .ok()?;
        lock_file.lock_exclusive().ok()?;
        Some(lock_file)
    }

    /// Load persisted session store (caller must hold lock).
    fn load_store_inner(&self) -> SessionStore {
        let Some(file) = fs::File::open(&self.state_file).ok() else {
            return SessionStore::default();
        };
        serde_json::from_reader(&file).unwrap_or_default()
    }

    /// Load persisted session store with lock.
    fn load_store(&self) -> SessionStore {
        let _lock = self.lock_state_file();
        self.load_store_inner()
    }

    /// Persist session store with exclusive lock and atomic write.
    /// Try to restore session state from persisted storage.
    fn restore_session(&self, session_id: &str) -> Option<StoredSession> {
        let store = self.load_store();
        store.sessions.get(session_id).cloned()
    }

    /// Persist a session binding (read-modify-write under single lock).
    fn persist_session(
        &self,
        session_id: &str,
        conversation_id: Option<&str>,
        emitted_line_count: usize,
    ) {
        let Some(_lock) = self.lock_state_file() else {
            return;
        };
        let mut store = self.load_store_inner();
        store.sessions.insert(
            session_id.to_string(),
            StoredSession {
                conversation_id: conversation_id.map(String::from),
                emitted_line_count,
            },
        );
        let tmp = self.state_file.with_extension("tmp");
        if let Ok(file) = fs::File::create(&tmp) {
            if serde_json::to_writer_pretty(&file, &store).is_ok() {
                let _ = fs::rename(&tmp, &self.state_file);
            }
        }
    }

    fn conversation_snapshot(&self) -> HashSet<String> {
        let Ok(entries) = fs::read_dir(&self.conversations_dir) else {
            return HashSet::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().map(|x| x == "pb").unwrap_or(false) {
                    path.file_stem().map(|s| s.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    fn new_conversation_id(&self, before: &HashSet<String>) -> Option<String> {
        let after = self.conversation_snapshot();
        let mut created: Vec<_> = after.difference(before).collect();
        if created.is_empty() {
            return None;
        }
        if created.len() > 1 {
            eprintln!(
                "[agy-acp] WARN: multiple new agy conversation files appeared; \
                 refusing to bind"
            );
            return None;
        }
        Some(created.remove(0).clone())
    }

    fn count_lines(text: &str) -> usize {
        if text.is_empty() {
            0
        } else {
            text.split_inclusive('\n').count()
        }
    }

    fn last_n_lines(text: &str, line_count: usize) -> String {
        if line_count == 0 || text.is_empty() {
            return String::new();
        }
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let start = lines.len().saturating_sub(line_count);
        lines[start..].concat()
    }

    fn extract_delta(
        emitted_line_count: usize,
        full_text: &str,
        conversation_bound: bool,
    ) -> String {
        if !conversation_bound || emitted_line_count == 0 {
            return full_text.to_string();
        }
        let lines: Vec<&str> = full_text.split_inclusive('\n').collect();
        if emitted_line_count <= lines.len() {
            return lines[emitted_line_count..].concat();
        }
        eprintln!(
            "[agy-acp] WARN: agy stdout line count shrank; \
             sending only the last 5 lines and resetting line-count baseline"
        );
        Self::last_n_lines(full_text, 5)
    }

    fn evict_if_needed(&mut self) {
        const MAX_SESSIONS: usize = 64;
        while self.sessions.len() >= MAX_SESSIONS {
            if let Some(key) = self.sessions.keys().next().cloned() {
                self.sessions.remove(&key);
            }
        }
    }

    fn restore_session_state(&mut self, session_id: &str) -> bool {
        let Some(stored) = self.restore_session(session_id) else {
            return false;
        };
        let Some(conversation_id) = stored.conversation_id else {
            return false;
        };
        // Evict only after confirming the restore target exists
        if !self.sessions.contains_key(session_id) {
            self.evict_if_needed();
        }
        self.sessions.insert(
            session_id.to_string(),
            Session {
                conversation_id: Some(conversation_id),
                emitted_line_count: stored.emitted_line_count,
            },
        );
        true
    }

    fn handle_initialize(&self, id: u64) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "agy", "version": env!("CARGO_PKG_VERSION") },
                "agentCapabilities": { "streaming": true, "loadSession": true },
            })),
            error: None,
        }
    }

    fn handle_session_new(&mut self, id: u64) -> JsonRpcResponse {
        let session_id = Uuid::new_v4().to_string();
        self.evict_if_needed();
        let conversation_id = None;
        self.sessions.insert(
            session_id.clone(),
            Session {
                conversation_id,
                emitted_line_count: 0,
            },
        );
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "sessionId": session_id })),
            error: None,
        }
    }

    fn handle_session_load(&mut self, id: u64, params: &Value) -> JsonRpcResponse {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if session_id.is_empty() {
            return JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(json!({"code":-32602,"message":"missing sessionId"})),
            };
        }

        if self.restore_session_state(session_id) {
            return JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({ "sessionId": session_id })),
                error: None,
            };
        }

        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({
                "code": -32000,
                "message": format!("unknown sessionId: {session_id}"),
            })),
        }
    }

    async fn handle_session_prompt(&mut self, id: u64, params: &Value) -> Vec<String> {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Restore evicted session from state file if needed
        if !session_id.is_empty() && !self.sessions.contains_key(session_id) {
            let _ = self.restore_session_state(session_id);
        }

        let prompt_text = params
            .get("prompt")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let clean_prompt = prompt_text.trim();

        // Take snapshot before spawning agy if we need to bind a conversation
        let snapshot = if self
            .sessions
            .get(session_id)
            .map(|s| s.conversation_id.is_none())
            .unwrap_or(false)
        {
            Some(self.conversation_snapshot())
        } else {
            None
        };

        // Build args
        let mut args: Vec<String> = Vec::new();
        args.push("--add-dir".to_string());
        args.push(self.working_dir.clone());
        if let Ok(extra) = std::env::var("AGY_EXTRA_ARGS") {
            args.extend(extra.split_whitespace().map(String::from));
        }
        if let Some(session) = self.sessions.get(session_id) {
            if let Some(conv_id) = &session.conversation_id {
                args.push("--conversation".to_string());
                args.push(conv_id.clone());
            }
        }
        args.push("-p".to_string());
        args.push(clean_prompt.to_string());

        let result = Command::new("agy")
            .args(&args)
            .current_dir(&self.working_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await;

        let mut output_lines = Vec::new();

        match result {
            Ok(output) => {
                // Log stderr if non-empty
                let stderr_text = String::from_utf8_lossy(&output.stderr);
                if !stderr_text.is_empty() {
                    eprintln!("[agy-acp] agy stderr: {}", stderr_text.trim_end());
                }

                if !output.status.success() {
                    eprintln!("[agy-acp] WARN: agy exited with status: {}", output.status);
                    if output.stdout.is_empty() {
                        let msg = if stderr_text.is_empty() {
                            format!("agy exited with status: {}", output.status)
                        } else {
                            format!("agy failed: {}", stderr_text.trim_end())
                        };
                        let resp = JsonRpcResponse {
                            jsonrpc: "2.0",
                            id,
                            result: None,
                            error: Some(json!({"code":-32000,"message":msg})),
                        };
                        output_lines.push(serde_json::to_string(&resp).unwrap());
                        return output_lines;
                    }
                }

                let full_text = String::from_utf8_lossy(&output.stdout).to_string();

                let emitted_line_count = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.emitted_line_count)
                    .unwrap_or(0);
                let conversation_bound = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.conversation_id.is_some())
                    .unwrap_or(false);
                let new_text =
                    Self::extract_delta(emitted_line_count, &full_text, conversation_bound);
                let full_text_line_count = Self::count_lines(&full_text);

                // Bind conversation from snapshot diff
                let conv_id = snapshot
                    .as_ref()
                    .and_then(|before| self.new_conversation_id(before));

                let mut persist_state: Option<(String, usize)> = None;
                if let Some(session) = self.sessions.get_mut(session_id) {
                    if session.conversation_id.is_none() {
                        session.conversation_id = conv_id.clone();
                    }
                    if session.conversation_id.is_some() {
                        session.emitted_line_count = full_text_line_count;
                        persist_state = session
                            .conversation_id
                            .clone()
                            .map(|cid| (cid, session.emitted_line_count));
                    } else {
                        session.emitted_line_count = 0;
                        eprintln!(
                            "[agy-acp] WARN: could not bind conversation ID; \
                             running in single-turn mode"
                        );
                    }
                }
                if let Some((cid, emitted_line_count)) = persist_state {
                    self.persist_session(session_id, Some(cid.as_str()), emitted_line_count);
                }

                let notification = serde_json::to_string(&JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: "session/update".to_string(),
                    params: json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": new_text },
                        },
                    }),
                })
                .unwrap();
                output_lines.push(notification);
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({ "stopReason": "end_turn" })),
                    error: None,
                };
                output_lines.push(serde_json::to_string(&resp).unwrap());
            }
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: None,
                    error: Some(json!({"code":-32000,"message":format!("failed to run agy: {e}")})),
                };
                output_lines.push(serde_json::to_string(&resp).unwrap());
            }
        }
        output_lines
    }
}

#[tokio::main]
async fn main() {
    let mut adapter = Adapter::new();

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
                _ => {}
            }
        }
    });

    let mut stdout = io::stdout();

    while let Some(line) = rx.recv().await {
        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let id = match req.id {
            Some(id) => id,
            None => continue,
        };

        let output = match req.method.as_deref() {
            Some("initialize") => {
                vec![serde_json::to_string(&adapter.handle_initialize(id)).unwrap()]
            }
            Some("session/new") => {
                vec![serde_json::to_string(&adapter.handle_session_new(id)).unwrap()]
            }
            Some("session/load") => {
                let params = req.params.unwrap_or(json!({}));
                vec![serde_json::to_string(&adapter.handle_session_load(id, &params)).unwrap()]
            }
            Some("session/prompt") => {
                let params = req.params.unwrap_or(json!({}));
                adapter.handle_session_prompt(id, &params).await
            }
            Some("session/cancel") => {
                let r = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({})),
                    error: None,
                };
                vec![serde_json::to_string(&r).unwrap()]
            }
            Some(method) => {
                let r = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: None,
                    error: Some(
                        json!({"code":-32601,"message":format!("method not found: {method}")}),
                    ),
                };
                vec![serde_json::to_string(&r).unwrap()]
            }
            None => continue,
        };

        for line in output {
            let _ = writeln!(stdout, "{}", line);
        }
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_delta_returns_full_text_when_unbound() {
        let result = Adapter::extract_delta(3, "oldnew", false);
        assert_eq!(result, "oldnew");
    }

    #[test]
    fn test_extract_delta_skips_emitted_lines_when_bound() {
        let result = Adapter::extract_delta(1, "first response\nsecond response", true);
        assert_eq!(result, "second response");
    }

    #[test]
    fn test_extract_delta_returns_empty_when_line_count_unchanged() {
        let result = Adapter::extract_delta(1, "fresh response", true);
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_delta_returns_full_when_line_count_shrinks() {
        let result = Adapter::extract_delta(7, "l1\nl2\nl3\nl4\nl5\nl6\n", true);
        assert_eq!(result, "l2\nl3\nl4\nl5\nl6\n");
    }

    #[test]
    fn test_extract_delta_preserves_leading_spaces() {
        let result = Adapter::extract_delta(1, "hello\n  indented code", true);
        assert_eq!(result, "  indented code");
    }

    #[test]
    fn test_extract_delta_returns_all_lines_when_fewer_than_five_on_fallback() {
        let result = Adapter::extract_delta(4, "l1\nl2\nl3\n", true);
        assert_eq!(result, "l1\nl2\nl3\n");
    }

    #[test]
    fn test_initialize_advertises_load_session_support() {
        let adapter = Adapter::new();
        let response = adapter.handle_initialize(1);
        assert_eq!(
            response
                .result
                .as_ref()
                .and_then(|r| r.get("agentCapabilities"))
                .and_then(|c| c.get("loadSession"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    #[ignore] // filesystem I/O — run with CHI_INTEG=1
    fn test_session_load_restores_persisted_session() {
        let root = std::env::temp_dir().join(format!("agy-acp-load-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&root);

        let mut adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: root.join("conversations"),
            state_file: root.join("sessions.json"),
        };
        adapter.persist_session("sess-1", Some("conv-abc"), 3);

        let response = adapter.handle_session_load(7, &json!({"sessionId": "sess-1"}));
        assert!(response.error.is_none());
        assert_eq!(
            adapter
                .sessions
                .get("sess-1")
                .and_then(|s| s.conversation_id.as_deref()),
            Some("conv-abc")
        );
        assert_eq!(
            adapter.sessions.get("sess-1").map(|s| s.emitted_line_count),
            Some(3)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore] // filesystem I/O — run with CHI_INTEG=1
    fn test_session_load_rejects_unknown_session() {
        let root = std::env::temp_dir().join(format!("agy-acp-missing-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&root);

        let mut adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: root.join("conversations"),
            state_file: root.join("sessions.json"),
        };

        let response = adapter.handle_session_load(9, &json!({"sessionId": "missing"}));
        assert!(response.result.is_none());
        assert_eq!(
            response
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str()),
            Some("unknown sessionId: missing")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore] // filesystem I/O — run with CHI_INTEG=1
    fn test_new_conversation_id_returns_none_when_multiple_files() {
        let root = std::env::temp_dir().join(format!("agy-acp-multi-{}", Uuid::new_v4()));
        let conv_dir = root.join("conversations");
        fs::create_dir_all(&conv_dir).unwrap();

        let adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: conv_dir.clone(),
            state_file: root.join("sessions.json"),
        };

        let before = adapter.conversation_snapshot();
        fs::write(conv_dir.join("a.pb"), b"").unwrap();
        fs::write(conv_dir.join("b.pb"), b"").unwrap();

        assert_eq!(adapter.new_conversation_id(&before), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore] // filesystem I/O — run with CHI_INTEG=1
    fn test_snapshot_diff_binds_single_new_conversation() {
        let root = std::env::temp_dir().join(format!("agy-acp-snap-{}", Uuid::new_v4()));
        let conv_dir = root.join("conversations");
        fs::create_dir_all(&conv_dir).unwrap();
        fs::write(conv_dir.join("existing.pb"), b"old").unwrap();

        let adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: conv_dir.clone(),
            state_file: root.join("sessions.json"),
        };

        let before = adapter.conversation_snapshot();
        fs::write(conv_dir.join("new-conv.pb"), b"new").unwrap();

        assert_eq!(
            adapter.new_conversation_id(&before),
            Some("new-conv".to_string())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore] // filesystem I/O — run with CHI_INTEG=1
    fn test_persist_and_restore_session_binding() {
        let root = std::env::temp_dir().join(format!("agy-acp-state-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&root);

        let adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: root.join("conversations"),
            state_file: root.join("sessions.json"),
        };

        adapter.persist_session("sess-1", Some("conv-abc"), 4);
        let restored = adapter.restore_session("sess-1");
        assert_eq!(
            restored.as_ref().and_then(|s| s.conversation_id.as_deref()),
            Some("conv-abc")
        );
        assert_eq!(restored.map(|s| s.emitted_line_count), Some(4));

        let missing = adapter.restore_session("sess-unknown");
        assert!(missing.is_none());

        let _ = fs::remove_dir_all(root);
    }
}

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

const DEFAULT_AGY_MODEL: &str = "Gemini 3.5 Flash (Medium)";

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
}

struct Session {
    conversation_id: Option<String>,
    /// Full stdout from the previous turn for prefix-checked delta extraction
    prev_output: String,
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
        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/tmp".to_string());
        let settings_file = PathBuf::from(&home).join(".gemini/antigravity-cli/settings.json");
        let default_model =
            std::env::var("AGY_DEFAULT_MODEL").unwrap_or_else(|_| DEFAULT_AGY_MODEL.to_string());
        Self::ensure_default_settings(&settings_file, &default_model, &working_dir);

        Self {
            sessions: HashMap::new(),
            working_dir,
            conversations_dir: PathBuf::from(&home).join(".gemini/antigravity-cli/conversations"),
            state_file: state_dir.join("sessions.json"),
        }
    }

    /// Seed Antigravity's native settings file when no model is configured.
    ///
    /// agy print mode does not expose a `--model` flag, so model selection is
    /// owned by ~/.gemini/antigravity-cli/settings.json. Preserve explicit
    /// operator choices; only write the OpenAB default for fresh PVCs or
    /// incomplete settings files.
    fn ensure_default_settings(settings_file: &PathBuf, default_model: &str, working_dir: &str) {
        if default_model.trim().is_empty() {
            return;
        }

        if let Some(parent) = settings_file.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("[agy-acp] WARN: failed to create settings dir: {err}");
                return;
            }
        }

        let mut settings = fs::read_to_string(settings_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({}));

        let mut changed = false;
        let has_model = settings
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_model {
            settings["model"] = json!(default_model);
            changed = true;
        }

        if !settings
            .get("trustedWorkspaces")
            .map(|v| v.is_array())
            .unwrap_or(false)
        {
            settings["trustedWorkspaces"] = json!([working_dir]);
            changed = true;
        }

        if changed {
            match serde_json::to_string_pretty(&settings) {
                Ok(body) => {
                    if let Err(err) = fs::write(settings_file, format!("{body}\n")) {
                        eprintln!("[agy-acp] WARN: failed to write settings: {err}");
                    }
                }
                Err(err) => eprintln!("[agy-acp] WARN: failed to serialize settings: {err}"),
            }
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
    /// Try to restore conversation_id from persisted state.
    fn restore_session(&self, session_id: &str) -> Option<String> {
        let store = self.load_store();
        store
            .sessions
            .get(session_id)
            .and_then(|s| s.conversation_id.clone())
    }

    /// Persist a session binding (read-modify-write under single lock).
    fn persist_session(&self, session_id: &str, conversation_id: Option<&str>) {
        let Some(_lock) = self.lock_state_file() else {
            return;
        };
        let mut store = self.load_store_inner();
        store.sessions.insert(
            session_id.to_string(),
            StoredSession {
                conversation_id: conversation_id.map(String::from),
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

    fn extract_delta(prev_output: &str, full_text: &str, conversation_bound: bool) -> String {
        if !conversation_bound || prev_output.is_empty() {
            return full_text.to_string();
        }
        if let Some(delta) = full_text.strip_prefix(prev_output) {
            return delta.trim_start_matches('\n').to_string();
        }
        eprintln!(
            "[agy-acp] WARN: agy stdout was not append-only; \
             sending full output and resetting delta baseline"
        );
        full_text.to_string()
    }

    fn handle_initialize(&self, id: u64) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "agy", "version": env!("CARGO_PKG_VERSION") },
                "agentCapabilities": { "streaming": true, "loadSession": false },
            })),
            error: None,
        }
    }

    fn handle_session_new(&mut self, id: u64) -> JsonRpcResponse {
        let session_id = Uuid::new_v4().to_string();
        // Evict an arbitrary session if at capacity (prevent unbounded growth)
        const MAX_SESSIONS: usize = 64;
        while self.sessions.len() >= MAX_SESSIONS {
            if let Some(key) = self.sessions.keys().next().cloned() {
                self.sessions.remove(&key);
            }
        }
        // Try to restore from persisted state (relevant if client reuses session IDs)
        let conversation_id = None;
        self.sessions.insert(
            session_id.clone(),
            Session {
                conversation_id,
                prev_output: String::new(),
            },
        );
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "sessionId": session_id })),
            error: None,
        }
    }

    async fn handle_session_prompt(&mut self, id: u64, params: &Value) -> Vec<String> {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Restore evicted session from state file if needed
        if !session_id.is_empty() && !self.sessions.contains_key(session_id) {
            if let Some(conv_id) = self.restore_session(session_id) {
                self.sessions.insert(
                    session_id.to_string(),
                    Session {
                        conversation_id: Some(conv_id),
                        prev_output: String::new(),
                    },
                );
            }
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

                let prev_output = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.prev_output.as_str())
                    .unwrap_or("");
                let conversation_bound = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.conversation_id.is_some())
                    .unwrap_or(false);
                let new_text = Self::extract_delta(prev_output, &full_text, conversation_bound);

                // Bind conversation from snapshot diff
                let conv_id = snapshot
                    .as_ref()
                    .and_then(|before| self.new_conversation_id(before));

                let mut persist_conv_id: Option<String> = None;
                if let Some(session) = self.sessions.get_mut(session_id) {
                    let newly_bound = session.conversation_id.is_none() && conv_id.is_some();
                    if session.conversation_id.is_none() {
                        session.conversation_id = conv_id.clone();
                    }
                    if session.conversation_id.is_some() {
                        session.prev_output = full_text;
                        if newly_bound {
                            persist_conv_id = session.conversation_id.clone();
                        }
                    } else {
                        session.prev_output.clear();
                        eprintln!(
                            "[agy-acp] WARN: could not bind conversation ID; \
                             running in single-turn mode"
                        );
                    }
                }
                if let Some(ref cid) = persist_conv_id {
                    self.persist_session(session_id, Some(cid.as_str()));
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
        let result = Adapter::extract_delta("old", "oldnew", false);
        assert_eq!(result, "oldnew");
    }

    #[test]
    fn test_extract_delta_strips_prefix_when_bound() {
        let result =
            Adapter::extract_delta("first response\n", "first response\nsecond response", true);
        assert_eq!(result, "second response");
    }

    #[test]
    fn test_extract_delta_returns_full_when_not_append_only() {
        let result = Adapter::extract_delta("old response", "fresh response", true);
        assert_eq!(result, "fresh response");
    }

    #[test]
    fn test_extract_delta_preserves_leading_spaces() {
        let result = Adapter::extract_delta("hello\n", "hello\n  indented code", true);
        assert_eq!(result, "  indented code");
    }

    #[test]
    fn test_ensure_default_settings_seeds_missing_model() {
        let root = std::env::temp_dir().join(format!("agy-acp-settings-{}", Uuid::new_v4()));
        let settings_file = root.join("settings.json");

        Adapter::ensure_default_settings(&settings_file, "Gemini 3.5 Flash (Medium)", "/work");

        let settings: Value =
            serde_json::from_str(&fs::read_to_string(&settings_file).unwrap()).unwrap();
        assert_eq!(
            settings.get("model").and_then(|v| v.as_str()),
            Some("Gemini 3.5 Flash (Medium)")
        );
        assert_eq!(
            settings
                .get("trustedWorkspaces")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .and_then(|v| v.as_str()),
            Some("/work")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_ensure_default_settings_preserves_existing_model() {
        let root = std::env::temp_dir().join(format!("agy-acp-settings-{}", Uuid::new_v4()));
        let settings_file = root.join("settings.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &settings_file,
            r#"{"model":"Claude Sonnet 4.6 (Thinking)","trustedWorkspaces":["/existing"]}"#,
        )
        .unwrap();

        Adapter::ensure_default_settings(&settings_file, "Gemini 3.5 Flash (Medium)", "/work");

        let settings: Value =
            serde_json::from_str(&fs::read_to_string(&settings_file).unwrap()).unwrap();
        assert_eq!(
            settings.get("model").and_then(|v| v.as_str()),
            Some("Claude Sonnet 4.6 (Thinking)")
        );
        assert_eq!(
            settings
                .get("trustedWorkspaces")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .and_then(|v| v.as_str()),
            Some("/existing")
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

        adapter.persist_session("sess-1", Some("conv-abc"));
        let restored = adapter.restore_session("sess-1");
        assert_eq!(restored, Some("conv-abc".to_string()));

        let missing = adapter.restore_session("sess-unknown");
        assert_eq!(missing, None);

        let _ = fs::remove_dir_all(root);
    }
}

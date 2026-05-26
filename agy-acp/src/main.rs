use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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

struct Session {
    /// agy conversation ID (from conversations directory)
    conversation_id: Option<String>,
    /// full stdout from the previous turn for prefix-checked delta extraction
    prev_output: String,
}

struct Adapter {
    sessions: HashMap<String, Session>,
    working_dir: String,
    conversations_dir: PathBuf,
}

impl Adapter {
    fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
            conversations_dir: PathBuf::from(&home).join(".gemini/antigravity-cli/conversations"),
        }
    }

    fn conversation_snapshot(&self) -> HashSet<String> {
        let Ok(entries) = std::fs::read_dir(&self.conversations_dir) else {
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
        let mut created = after.difference(before);
        let first = created.next()?.to_string();
        if created.next().is_some() {
            eprintln!(
                "[agy-acp] WARN: multiple new agy conversation files appeared; \
                 refusing to bind this ACP session by global heuristic"
            );
            return None;
        }
        Some(first)
    }

    fn extract_delta(prev_output: &str, full_text: &str, conversation_bound: bool) -> String {
        if !conversation_bound || prev_output.is_empty() {
            return full_text.to_string();
        }

        if let Some(delta) = full_text.strip_prefix(prev_output) {
            return delta.trim_start().to_string();
        }

        eprintln!(
            "[agy-acp] WARN: agy stdout was not append-only for the bound conversation; \
             sending full output for this turn and resetting delta baseline"
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
        self.sessions.insert(
            session_id.clone(),
            Session {
                conversation_id: None,
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

        let conversation_snapshot = if self
            .sessions
            .get(session_id)
            .map(|s| s.conversation_id.is_none())
            .unwrap_or(false)
        {
            Some(self.conversation_snapshot())
        } else {
            None
        };

        // Build args: use --conversation <ID> for subsequent turns
        let mut args: Vec<String> = Vec::new();
        // Always add working dir as workspace so agy reads AGENTS.md/GEMINI.md
        args.push("--add-dir".to_string());
        args.push(self.working_dir.clone());
        // Add extra args from AGY_EXTRA_ARGS env var if set
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
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        let mut output_lines = Vec::new();

        match result {
            Ok(output) => {
                let full_text = String::from_utf8_lossy(&output.stdout).to_string();

                let prev_output = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.prev_output.clone())
                    .unwrap_or_default();
                let conversation_bound = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.conversation_id.is_some())
                    .unwrap_or(false);
                let new_text = Self::extract_delta(&prev_output, &full_text, conversation_bound);

                let conv_id = conversation_snapshot
                    .as_ref()
                    .and_then(|before| self.new_conversation_id(before));

                if let Some(session) = self.sessions.get_mut(session_id) {
                    if session.conversation_id.is_none() {
                        session.conversation_id = conv_id;
                    }
                    if session.conversation_id.is_some() {
                        session.prev_output = full_text;
                    } else {
                        session.prev_output.clear();
                        eprintln!(
                            "[agy-acp] WARN: could not bind an agy conversation ID; \
                             this ACP session will run in single-turn mode until a \
                             conversation can be bound"
                        );
                    }
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
    use std::fs;

    #[test]
    fn extract_delta_returns_full_text_without_bound_conversation() {
        let output = Adapter::extract_delta("old", "oldnew", false);
        assert_eq!(output, "oldnew");
    }

    #[test]
    fn extract_delta_returns_only_appended_output_for_bound_conversation() {
        let output =
            Adapter::extract_delta("first response\n", "first response\nsecond response", true);
        assert_eq!(output, "second response");
    }

    #[test]
    fn extract_delta_falls_back_when_output_is_not_append_only() {
        let output = Adapter::extract_delta("old response", "fresh response", true);
        assert_eq!(output, "fresh response");
    }

    #[test]
    fn new_conversation_id_uses_snapshot_diff_instead_of_latest_file() {
        let root = std::env::temp_dir().join(format!("agy-acp-test-{}", Uuid::new_v4()));
        let conversations_dir = root.join("conversations");
        fs::create_dir_all(&conversations_dir).unwrap();
        fs::write(conversations_dir.join("old.pb"), b"old").unwrap();

        let adapter = Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: conversations_dir.clone(),
        };

        let before = adapter.conversation_snapshot();
        fs::write(conversations_dir.join("new.pb"), b"new").unwrap();

        assert_eq!(
            adapter.new_conversation_id(&before),
            Some("new".to_string())
        );

        let _ = fs::remove_dir_all(root);
    }
}

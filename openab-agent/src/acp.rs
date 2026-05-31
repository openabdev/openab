use crate::agent::Agent;
use crate::llm::AnthropicProvider;
#[cfg(feature = "mcp")]
use crate::mcp::{self, McpRuntimeManager};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<u64>,
    pub method: Option<String>,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

pub struct AcpServer {
    // TODO(v0.2): add session TTL and periodic cleanup to prevent OOM
    sessions: HashMap<String, Agent>,
    working_dir: String,
    #[cfg(feature = "mcp")]
    mcp_manager: Option<McpRuntimeManager>,
}

impl AcpServer {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
            #[cfg(feature = "mcp")]
            mcp_manager: mcp::load_runtime_or_warn(),
        }
    }

    pub async fn run(&mut self) {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        std::thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                #[allow(clippy::collapsible_match)]
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
                Some("initialize") => vec![self.handle_initialize(id)],
                Some("session/new") => vec![self.handle_session_new(id)],
                Some("session/prompt") => {
                    let params = req.params.unwrap_or(json!({}));
                    self.handle_session_prompt(id, &params).await
                }
                Some("session/cancel") => {
                    // TODO(v0.2): implement cancellation token to abort in-progress agent.run()
                    vec![self.ok_response(id, json!({}))]
                }
                Some(method) => {
                    vec![self.error_response(id, -32601, &format!("method not found: {method}"))]
                }
                None => continue,
            };

            for line in output {
                let _ = writeln!(stdout, "{}", line);
            }
            let _ = stdout.flush();
        }
    }

    fn handle_initialize(&self, id: u64) -> String {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": 1,
                "agentInfo": {
                    "name": "openab-agent",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "agentCapabilities": {
                    "streaming": false,
                    "loadSession": false
                }
            })),
            error: None,
        };
        serde_json::to_string(&resp).unwrap()
    }

    fn handle_session_new(&mut self, id: u64) -> String {
        let session_id = Uuid::new_v4().to_string();

        // Respect OPENAB_AGENT_PROVIDER if set, otherwise auto-detect
        let provider_choice = std::env::var("OPENAB_AGENT_PROVIDER").unwrap_or_default();
        let provider: Box<dyn crate::llm::LlmProvider> = match provider_choice.as_str() {
            "anthropic" => match AnthropicProvider::from_env() {
                Ok(p) => Box::new(p),
                Err(e) => return self.error_response(id, -32000, &e),
            },
            "openai" | "codex" => match crate::llm::OpenAiProvider::from_auth_store() {
                Ok(p) => Box::new(p),
                Err(e) => return self.error_response(id, -32000, &e),
            },
            _ => {
                // Auto-detect: try API key first, then OAuth token
                match AnthropicProvider::from_env() {
                    Ok(p) => Box::new(p),
                    Err(_) => match crate::llm::OpenAiProvider::from_auth_store() {
                        Ok(p) => Box::new(p),
                        Err(e) => {
                            return self.error_response(
                                id,
                                -32000,
                                &format!("No credentials: set ANTHROPIC_API_KEY or run `openab-agent auth codex-oauth`. {e}"),
                            )
                        }
                    },
                }
            }
        };

        let agent = Agent::new_boxed(
            provider,
            self.working_dir.clone(),
            #[cfg(feature = "mcp")]
            self.mcp_manager.clone(),
        );
        self.sessions.insert(session_id.clone(), agent);
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "sessionId": session_id })),
            error: None,
        };
        serde_json::to_string(&resp).unwrap()
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

        if prompt_text.trim().is_empty() {
            return vec![self.error_response(id, -32602, "prompt is empty")];
        }

        let agent = match self.sessions.get_mut(session_id) {
            Some(a) => a,
            None => {
                return vec![self.error_response(id, -32600, "unknown session")];
            }
        };

        let mut output_lines = Vec::new();
        let session_id_owned = session_id.to_string();

        match agent.run(&prompt_text).await {
            Ok(response_text) => {
                let notification = serde_json::to_string(&JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: "session/update".to_string(),
                    params: json!({
                        "sessionId": session_id_owned,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": response_text }
                        }
                    }),
                })
                .unwrap();
                output_lines.push(notification);
                output_lines.push(self.ok_response(id, json!({ "stopReason": "end_turn" })));
            }
            Err(e) => {
                output_lines.push(self.error_response(id, -32000, &format!("agent error: {e}")));
            }
        }

        output_lines
    }

    fn ok_response(&self, id: u64, result: Value) -> String {
        serde_json::to_string(&JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        })
        .unwrap()
    }

    fn error_response(&self, id: u64, code: i64, message: &str) -> String {
        serde_json::to_string(&JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({ "code": code, "message": message })),
        })
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate process-global env vars (notably
    /// `ANTHROPIC_API_KEY`). Without this, `test_session_new` and
    /// `test_session_new_missing_key` race on the same key when run in
    /// parallel — set/remove from one thread is observed by the other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_initialize_response() {
        let server = AcpServer::new();
        let resp_str = server.handle_initialize(1);
        let resp: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["agentInfo"]["name"], "openab-agent");
        assert_eq!(resp["result"]["agentCapabilities"]["streaming"], false);
    }

    #[test]
    fn test_session_new() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Set a fake key so from_env() succeeds in CI
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };
        let mut server = AcpServer::new();
        let resp_str = server.handle_session_new(2);
        let resp: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert!(resp["result"]["sessionId"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn test_session_new_missing_key() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Ensure no OAuth token exists either
        let auth_path =
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
                .join(".openab/agent/auth.json");
        let _ = std::fs::remove_file(&auth_path);
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let mut server = AcpServer::new();
        let resp_str = server.handle_session_new(3);
        let resp: Value = serde_json::from_str(&resp_str).unwrap();
        assert!(resp["error"].is_object());
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ANTHROPIC_API_KEY"));
    }
}

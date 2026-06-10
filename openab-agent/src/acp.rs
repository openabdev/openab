use crate::agent::Agent;
use crate::mcp::{self, McpRuntimeManager};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
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

/// Pending agent→host requests keyed by the outbound JSON-RPC id. Each entry
/// is the `oneshot` half that wakes the awaiting caller once the host's
/// response with the matching id arrives back over stdin.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Value>>>>>;

/// Duplex channel from the MCP client layer back into the ACP loop.
///
/// The ACP transport (`AcpServer::run`) is otherwise half-duplex: it answers
/// inbound host→agent requests and emits fire-and-forget `session/update`
/// notifications. `HostBridge` adds the missing agent→host *request/response*
/// direction so an MCP `ClientHandler` running on an rmcp task can ask the
/// host a question (e.g. elicitation form) and await a structured reply.
///
/// All outbound bytes funnel through `writer` (a single stdout-owning drain
/// task) to preserve the one-writer invariant; `pending` correlates each
/// outbound id with its awaiting `oneshot`; `next_id` mints monotonic ids.
#[derive(Clone, Debug)]
pub struct HostBridge {
    writer: mpsc::UnboundedSender<String>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
}

impl HostBridge {
    pub fn new(writer: mpsc::UnboundedSender<String>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Send an outbound agent→host request and await the host's response.
    /// Returns `Ok(result)` / `Err(error)` mirroring JSON-RPC. Returns `Err`
    /// (rather than blocking forever) when no host is listening or the channel
    /// is closed, so callers can degrade gracefully (e.g. auto-decline).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("host bridge pending map poisoned")
            .insert(id, reply_tx);

        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();

        if self.writer.send(line).is_err() {
            self.pending
                .lock()
                .expect("host bridge pending map poisoned")
                .remove(&id);
            return Err(json!({ "code": -32603, "message": "host channel closed" }));
        }

        match reply_rx.await {
            Ok(outcome) => outcome,
            Err(_) => Err(json!({ "code": -32603, "message": "host reply dropped" })),
        }
    }

    /// If `line` is an inbound JSON-RPC *response* to one of our outbound
    /// requests, resolve the matching pending `oneshot` and return `true`.
    /// Returns `false` for anything else (inbound requests, notifications,
    /// unknown / already-completed ids) so the caller falls through to the
    /// normal request-dispatch path.
    pub fn try_resolve_response(&self, line: &str) -> bool {
        let val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return false,
        };
        // A response carries an id + (result | error) and NO method. A request
        // or notification has a method — leave those for the dispatch loop.
        if val.get("method").is_some() {
            return false;
        }
        let Some(id) = val.get("id").and_then(|v| v.as_u64()) else {
            return false;
        };
        let Some(reply_tx) = self
            .pending
            .lock()
            .expect("host bridge pending map poisoned")
            .remove(&id)
        else {
            return false;
        };
        let outcome = if let Some(err) = val.get("error") {
            Err(err.clone())
        } else {
            Ok(val.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = reply_tx.send(outcome);
        true
    }
}

pub struct AcpServer {
    // TODO(v0.2): add session TTL and periodic cleanup to prevent OOM
    sessions: HashMap<String, Agent>,
    working_dir: String,
    mcp_manager: Option<McpRuntimeManager>,
}

impl AcpServer {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
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

        // Single stdout owner: every outbound line (dispatch responses,
        // notifications, and agent→host requests from rmcp tasks) funnels
        // through `out_tx` into this one drain task, preserving the
        // one-writer invariant the HostBridge relies on.
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut stdout = io::stdout();
            while let Some(line) = out_rx.recv().await {
                let _ = writeln!(stdout, "{}", line);
                let _ = stdout.flush();
            }
        });

        // Built now so its writer half is shared with the drain task, then
        // injected into the MCP manager *before* the first `session/new` clones
        // it into an Agent — so every session's MCP connections inherit a live
        // host bridge for elicitation. Inbound host replies are routed through
        // `try_resolve_response` below.
        let bridge = HostBridge::new(out_tx.clone());
        if let Some(manager) = self.mcp_manager.as_mut() {
            manager.set_host_bridge(bridge.clone());
            manager.start_eviction_loop();
        }

        while let Some(line) = rx.recv().await {
            // Intercept host→agent responses to our outbound requests before
            // the request-dispatch path; everything else falls through.
            if bridge.try_resolve_response(&line) {
                continue;
            }

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
                let _ = out_tx.send(line);
            }
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

        // Respect OPENAB_AGENT_PROVIDER if set, otherwise auto-detect. Shared
        // with the MCP sampling path via `llm::select_provider`.
        let provider_choice = std::env::var("OPENAB_AGENT_PROVIDER").unwrap_or_default();
        let provider: Box<dyn crate::llm::LlmProvider> =
            match crate::llm::select_provider(&provider_choice) {
                Ok(p) => p,
                Err(e) => return self.error_response(id, -32000, &e),
            };

        let agent = Agent::new_boxed(provider, self.working_dir.clone(), self.mcp_manager.clone());
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
        let resp_str = temp_env::with_vars(
            [
                ("ANTHROPIC_API_KEY", Some("test-key")),
                ("OPENAB_AGENT_PROVIDER", None),
            ],
            || {
                let mut server = AcpServer::new();
                server.handle_session_new(2)
            },
        );
        let resp: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert!(resp["result"]["sessionId"].as_str().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn test_host_bridge_request_resolves_on_matching_response() {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let bridge = HostBridge::new(out_tx);

        let task = {
            let bridge = bridge.clone();
            tokio::spawn(async move {
                bridge
                    .request("session/request_permission", json!({}))
                    .await
            })
        };

        // Drain the outbound request line and echo back a response with the
        // same id, simulating the host.
        let line = out_rx.recv().await.unwrap();
        let sent: Value = serde_json::from_str(&line).unwrap();
        let id = sent["id"].as_u64().unwrap();
        assert_eq!(sent["method"], "session/request_permission");
        let resolved = bridge.try_resolve_response(
            &json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } }).to_string(),
        );
        assert!(resolved);

        let outcome = task.await.unwrap();
        assert_eq!(outcome.unwrap(), json!({ "ok": true }));
    }

    #[tokio::test]
    async fn test_host_bridge_request_errors_on_closed_channel() {
        let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
        drop(out_rx); // no drain → send fails
        let bridge = HostBridge::new(out_tx);
        let outcome = bridge
            .request("session/request_permission", json!({}))
            .await;
        let err = outcome.unwrap_err();
        assert_eq!(err["code"], -32603);
    }

    #[tokio::test]
    async fn test_host_bridge_resolves_error_response() {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let bridge = HostBridge::new(out_tx);
        let task = {
            let bridge = bridge.clone();
            tokio::spawn(async move { bridge.request("m", json!({})).await })
        };
        let line = out_rx.recv().await.unwrap();
        let id: u64 = serde_json::from_str::<Value>(&line).unwrap()["id"]
            .as_u64()
            .unwrap();
        let resolved = bridge.try_resolve_response(
            &json!({ "id": id, "error": { "code": -1, "message": "nope" } }).to_string(),
        );
        assert!(resolved);
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err["code"], -1);
    }

    #[test]
    fn test_host_bridge_ignores_unknown_id_and_requests() {
        let (out_tx, _out_rx) = mpsc::unbounded_channel::<String>();
        let bridge = HostBridge::new(out_tx);
        // Unknown id → not ours.
        assert!(!bridge.try_resolve_response(&json!({ "id": 999, "result": {} }).to_string()));
        // Has a method → it's a request/notification, not a response.
        assert!(!bridge.try_resolve_response(
            &json!({ "id": 1, "method": "initialize", "params": {} }).to_string()
        ));
        // Not JSON → ignored.
        assert!(!bridge.try_resolve_response("not json"));
    }

    #[test]
    fn test_session_new_missing_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().to_string();
        let resp_str = temp_env::with_vars(
            [
                ("ANTHROPIC_API_KEY", None),
                ("OPENAB_AGENT_PROVIDER", None),
                ("HOME", Some(home.as_str())),
            ],
            || {
                let mut server = AcpServer::new();
                server.handle_session_new(3)
            },
        );
        let resp: Value = serde_json::from_str(&resp_str).unwrap();
        assert!(resp["error"].is_object());
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ANTHROPIC_API_KEY"));
    }
}

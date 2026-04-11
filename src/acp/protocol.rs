use serde::{Deserialize, Serialize};
use serde_json::Value;

// --- Outgoing ---

#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id, method: method.into(), params }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub result: Value,
}

impl JsonRpcResponse {
    pub fn new(id: u64, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result }
    }
}

// --- Incoming ---

#[derive(Debug, Deserialize)]
pub struct JsonRpcMessage {
    pub id: Option<u64>,
    pub method: Option<String>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

// --- ACP notification classification ---

#[derive(Debug)]
pub enum AcpEvent {
    Text(String),
    Thinking,
    ToolStart { id: String, title: String },
    ToolDone { id: String, title: String, status: String },
    Status,
}

pub fn classify_notification(msg: &JsonRpcMessage) -> Option<AcpEvent> {
    let params = msg.params.as_ref()?;
    let update = params.get("update")?;
    let session_update = update.get("sessionUpdate")?.as_str()?;

    // toolCallId is the stable identity across tool_call → tool_call_update
    // events for the same tool invocation. claude-agent-acp emits the first
    // event before the input fields are streamed in (so the title falls back
    // to "Terminal" / "Edit" / etc.) and refines them in a later
    // tool_call_update; without the id we can't tell those events belong to
    // the same call and end up rendering placeholder + refined as two
    // separate lines.
    let tool_id = update
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match session_update {
        "agent_message_chunk" => {
            let text = update.get("content")?.get("text")?.as_str()?;
            Some(AcpEvent::Text(text.to_string()))
        }
        "agent_thought_chunk" => {
            Some(AcpEvent::Thinking)
        }
        "tool_call" => {
            let title = update.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(AcpEvent::ToolStart { id: tool_id, title })
        }
        "tool_call_update" => {
            let title = update.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if status == "completed" || status == "failed" {
                Some(AcpEvent::ToolDone { id: tool_id, title, status })
            } else {
                Some(AcpEvent::ToolStart { id: tool_id, title })
            }
        }
        "plan" => Some(AcpEvent::Status),
        _ => None,
    }
}

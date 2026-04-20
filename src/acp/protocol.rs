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

// --- ACP configOptions (session-level configuration) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigOptionValue {
    pub value: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(rename = "type")]
    pub option_type: String,
    pub current_value: String,
    pub options: Vec<ConfigOptionValue>,
}

/// Extract configOptions from a JSON-RPC result value.
/// Supports standard `configOptions` and kiro-cli's `models`/`modes` fallback.
pub fn parse_config_options(result: &Value) -> Vec<ConfigOption> {
    if let Some(opts) = result
        .get("configOptions")
        .and_then(|v| serde_json::from_value::<Vec<ConfigOption>>(v.clone()).ok())
    {
        if !opts.is_empty() {
            return opts;
        }
    }

    // Kiro-cli fallback: parse models/modes format
    let mut options = Vec::new();

    if let Some(models) = result.get("models") {
        let current = models.get("currentModelId").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(available) = models.get("availableModels").and_then(|v| v.as_array()) {
            let values: Vec<ConfigOptionValue> = available
                .iter()
                .filter_map(|m| {
                    let id = m.get("modelId").or_else(|| m.get("id")).and_then(|v| v.as_str())?;
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some(ConfigOptionValue {
                        value: id.to_string(),
                        name: name.to_string(),
                        description: m.get("description").and_then(|v| v.as_str()).map(String::from),
                    })
                })
                .collect();
            if !values.is_empty() {
                options.push(ConfigOption {
                    id: "model".to_string(),
                    name: "Model".to_string(),
                    description: Some("AI model selection".to_string()),
                    category: Some("model".to_string()),
                    option_type: "enum".to_string(),
                    current_value: current.to_string(),
                    options: values,
                });
            }
        }
    }

    if let Some(modes) = result.get("modes") {
        let current = modes.get("currentModeId").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(available) = modes.get("availableModes").and_then(|v| v.as_array()) {
            let values: Vec<ConfigOptionValue> = available
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(|v| v.as_str())?;
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some(ConfigOptionValue {
                        value: id.to_string(),
                        name: name.to_string(),
                        description: m.get("description").and_then(|v| v.as_str()).map(String::from),
                    })
                })
                .collect();
            if !values.is_empty() {
                options.push(ConfigOption {
                    id: "agent".to_string(),
                    name: "Agent".to_string(),
                    description: Some("Agent mode selection".to_string()),
                    category: Some("agent".to_string()),
                    option_type: "enum".to_string(),
                    current_value: current.to_string(),
                    options: values,
                });
            }
        }
    }

    options
}

// --- ACP notification classification ---

#[derive(Debug)]
pub enum AcpEvent {
    Text(String),
    Thinking,
    ToolStart { id: String, title: String },
    ToolDone { id: String, title: String, status: String },
    ConfigUpdate { options: Vec<ConfigOption> },
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
        "config_option_update" => {
            let options = parse_config_options(update);
            Some(AcpEvent::ConfigUpdate { options })
        }
        _ => None,
    }
}

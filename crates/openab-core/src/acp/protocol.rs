use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(u64),
    String(String),
}

impl JsonRpcId {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(id) => Some(*id),
            Self::String(_) => None,
        }
    }
}

impl From<u64> for JsonRpcId {
    fn from(id: u64) -> Self {
        Self::Number(id)
    }
}

impl From<String> for JsonRpcId {
    fn from(id: String) -> Self {
        Self::String(id)
    }
}

impl From<&str> for JsonRpcId {
    fn from(id: &str) -> Self {
        Self::String(id.to_string())
    }
}

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
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: JsonRpcId,
    pub result: Value,
}

impl JsonRpcResponse {
    pub fn new(id: impl Into<JsonRpcId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: id.into(),
            result,
        }
    }
}

// --- Incoming ---

#[derive(Debug, Deserialize)]
pub struct JsonRpcMessage {
    pub id: Option<JsonRpcId>,
    pub method: Option<String>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    /// Optional structured data from the agent (JSON-RPC `error.data`).
    /// Agents like codex-acp include `{"message": "...", "codex_error_info": "..."}`.
    pub data: Option<Value>,
}

impl JsonRpcError {
    /// Extract a human-readable detail from `error.data.message` if present.
    ///
    /// The `"message"` key is a convention used by codex-acp and aligns with
    /// common JSON-RPC practice, but is NOT mandated by the ACP spec.
    /// Other agents may use `"detail"`, `"reason"`, etc. — extend here if needed.
    pub fn data_message(&self) -> Option<&str> {
        self.data
            .as_ref()
            .and_then(|d| d.get("message"))
            .and_then(|m| m.as_str())
    }
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)?;
        if let Some(detail) = self.data_message() {
            write!(f, " — {detail}")?;
        }
        Ok(())
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
        let current = models
            .get("currentModelId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(available) = models.get("availableModels").and_then(|v| v.as_array()) {
            let values: Vec<ConfigOptionValue> = available
                .iter()
                .filter_map(|m| {
                    let id = m
                        .get("modelId")
                        .or_else(|| m.get("id"))
                        .and_then(|v| v.as_str())?;
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some(ConfigOptionValue {
                        value: id.to_string(),
                        name: name.to_string(),
                        description: m
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from),
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
        let current = modes
            .get("currentModeId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(available) = modes.get("availableModes").and_then(|v| v.as_array()) {
            let values: Vec<ConfigOptionValue> = available
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(|v| v.as_str())?;
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    Some(ConfigOptionValue {
                        value: id.to_string(),
                        name: name.to_string(),
                        description: m
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(String::from),
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

// --- ACP prompt result parsing ---

/// Parsed fields from the `session/prompt` final response `result` object.
/// All fields are optional — agents may omit `usage` entirely.
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl TurnResult {
    /// Returns true when the turn ended normally but produced zero output —
    /// a strong signal of silent provider/auth failure.
    pub fn is_silent_failure(&self) -> bool {
        matches!(
            (self.stop_reason.as_deref(), self.output_tokens),
            (Some("end_turn"), Some(0))
        )
    }
}

/// Parse `stopReason` and `usage` from a `session/prompt` result value.
pub fn parse_turn_result(result: &Value) -> TurnResult {
    let stop_reason = result
        .get("stopReason")
        .and_then(|v| v.as_str())
        .map(String::from);
    let usage = result.get("usage");
    let input_tokens = usage.and_then(|u| u.get("inputTokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("outputTokens")).and_then(|v| v.as_u64());
    let total_tokens = usage.and_then(|u| u.get("totalTokens")).and_then(|v| v.as_u64());
    TurnResult {
        stop_reason,
        input_tokens,
        output_tokens,
        total_tokens,
    }
}

// --- Account usage report (kiro-cli `_kiro.dev/commands/execute` extension) ---

/// One resource-type usage breakdown (e.g. credits) from a usage report.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageBreakdown {
    pub display_name: String,
    pub used: f64,
    /// Plan allowance for this resource. `None` when the account has no
    /// per-user cap (`hasLimit: false`, e.g. pooled enterprise credits).
    pub limit: Option<f64>,
    pub percentage: Option<u64>,
    /// Accrued overage charges for this cycle, if any.
    pub overage_charges: Option<f64>,
    pub currency: Option<String>,
}

/// Account-level usage/billing report returned by kiro-cli's `/usage` command
/// when executed over ACP via `_kiro.dev/commands/execute`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageReport {
    pub plan_name: String,
    /// Billing cycle reset date (e.g. "2026-08-01"), if reported.
    pub billing_cycle_reset: Option<String>,
    pub breakdowns: Vec<UsageBreakdown>,
}

/// Parse a usage report from the result of a `_kiro.dev/commands/execute`
/// request for the `usage` command. Expected shape (kiro-cli 2.12.x):
///
/// ```json
/// {"success": true, "message": "...", "data": {
///    "planName": "KIRO POWER", "billingCycleReset": "2026-08-01",
///    "usageBreakdowns": [{"displayName": "Credits", "used": 128.5,
///        "limit": 10000.0, "percentage": 1, "hasLimit": true,
///        "overageCharges": 0.0, "currency": "USD"}]}}
/// ```
///
/// Returns `None` when `success` is not true or the data shape is missing —
/// callers should treat that as "usage not supported by this agent".
pub fn parse_usage_report(result: &Value) -> Option<UsageReport> {
    if !result.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let data = result.get("data")?;
    let plan_name = data.get("planName")?.as_str()?.to_string();
    let billing_cycle_reset = data
        .get("billingCycleReset")
        .and_then(|v| v.as_str())
        .map(String::from);

    let breakdowns = data
        .get("usageBreakdowns")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|b| {
                    // `hasLimit: false` is an explicit sentinel (pooled/no-cap
                    // enterprise accounts). Some kiro-cli versions omit the
                    // field entirely while still returning a numeric `limit`
                    // (observed live: overage state on kiro-cli in the pr1392
                    // image) — treat a missing `hasLimit` as "has a limit if a
                    // numeric limit is present" instead of hiding the cap.
                    let has_limit = b
                        .get("hasLimit")
                        .and_then(|v| v.as_bool())
                        .unwrap_or_else(|| {
                            b.get("limit").and_then(|v| v.as_f64()).is_some()
                        });
                    Some(UsageBreakdown {
                        display_name: b.get("displayName")?.as_str()?.to_string(),
                        used: b.get("used")?.as_f64()?,
                        limit: if has_limit {
                            b.get("limit").and_then(|v| v.as_f64())
                        } else {
                            None
                        },
                        percentage: b.get("percentage").and_then(|v| v.as_u64()),
                        overage_charges: b.get("overageCharges").and_then(|v| v.as_f64()),
                        currency: b.get("currency").and_then(|v| v.as_str()).map(String::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(UsageReport {
        plan_name,
        billing_cycle_reset,
        breakdowns,
    })
}

// --- ACP notification classification ---

#[derive(Debug)]
pub enum AcpEvent {
    Text(String),
    Thinking,
    ToolStart {
        id: String,
        title: String,
    },
    ToolDone {
        id: String,
        title: String,
        status: String,
    },
    ConfigUpdate {
        options: Vec<ConfigOption>,
    },
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
        "agent_thought_chunk" => Some(AcpEvent::Thinking),
        "tool_call" => {
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AcpEvent::ToolStart { id: tool_id, title })
        }
        "tool_call_update" => {
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if status == "completed" || status == "failed" {
                Some(AcpEvent::ToolDone {
                    id: tool_id,
                    title,
                    status,
                })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_standard_config_options() {
        let result = json!({
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "type": "enum",
                "currentValue": "claude-sonnet-4",
                "options": [
                    {"value": "claude-sonnet-4", "name": "Sonnet 4"},
                    {"value": "claude-opus-4", "name": "Opus 4"}
                ]
            }]
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, "model");
        assert_eq!(opts[0].current_value, "claude-sonnet-4");
        assert_eq!(opts[0].options.len(), 2);
    }

    #[test]
    fn parse_kiro_models_fallback() {
        let result = json!({
            "models": {
                "currentModelId": "m1",
                "availableModels": [
                    {"modelId": "m1", "name": "Model One"},
                    {"modelId": "m2", "name": "Model Two"}
                ]
            }
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, "model");
        assert_eq!(opts[0].category.as_deref(), Some("model"));
        assert_eq!(opts[0].current_value, "m1");
        assert_eq!(opts[0].options.len(), 2);
    }

    #[test]
    fn parse_kiro_modes_fallback() {
        let result = json!({
            "modes": {
                "currentModeId": "default",
                "availableModes": [
                    {"id": "default", "name": "Default"},
                    {"id": "planner", "name": "Planner"}
                ]
            }
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, "agent");
        assert_eq!(opts[0].category.as_deref(), Some("agent"));
        assert_eq!(opts[0].current_value, "default");
    }

    #[test]
    fn parse_kiro_models_and_modes() {
        let result = json!({
            "models": {
                "currentModelId": "m1",
                "availableModels": [{"modelId": "m1", "name": "M1"}]
            },
            "modes": {
                "currentModeId": "default",
                "availableModes": [{"id": "default", "name": "Default"}]
            }
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, "model");
        assert_eq!(opts[1].id, "agent");
    }

    #[test]
    fn parse_standard_takes_precedence_over_kiro() {
        let result = json!({
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "type": "enum",
                "currentValue": "standard",
                "options": [{"value": "standard", "name": "Standard"}]
            }],
            "models": {
                "currentModelId": "kiro",
                "availableModels": [{"modelId": "kiro", "name": "Kiro"}]
            }
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].current_value, "standard");
    }

    #[test]
    fn parse_empty_result() {
        let opts = parse_config_options(&json!({}));
        assert!(opts.is_empty());
    }

    #[test]
    fn parse_empty_config_options_falls_through_to_kiro() {
        let result = json!({
            "configOptions": [],
            "models": {
                "currentModelId": "m1",
                "availableModels": [{"modelId": "m1", "name": "M1"}]
            }
        });
        let opts = parse_config_options(&result);
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, "model");
    }

    // --- parse_turn_result tests ---

    #[test]
    fn turn_result_silent_failure() {
        let result = json!({"stopReason": "end_turn", "usage": {"inputTokens": 0, "outputTokens": 0, "totalTokens": 0}});
        let tr = parse_turn_result(&result);
        assert_eq!(tr.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(tr.output_tokens, Some(0));
        assert!(tr.is_silent_failure());
    }

    #[test]
    fn turn_result_silent_failure_with_nonzero_input() {
        let result = json!({"stopReason": "end_turn", "usage": {"inputTokens": 150, "outputTokens": 0, "totalTokens": 150}});
        let tr = parse_turn_result(&result);
        assert_eq!(tr.input_tokens, Some(150));
        assert_eq!(tr.output_tokens, Some(0));
        assert!(tr.is_silent_failure());
    }

    #[test]
    fn turn_result_nonzero_output_not_failure() {
        let result = json!({"stopReason": "end_turn", "usage": {"inputTokens": 10, "outputTokens": 50, "totalTokens": 60}});
        let tr = parse_turn_result(&result);
        assert!(!tr.is_silent_failure());
    }

    #[test]
    fn turn_result_missing_usage_not_failure() {
        let result = json!({"stopReason": "end_turn"});
        let tr = parse_turn_result(&result);
        assert_eq!(tr.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(tr.output_tokens, None);
        assert!(!tr.is_silent_failure());
    }

    #[test]
    fn turn_result_empty_object() {
        let tr = parse_turn_result(&json!({}));
        assert_eq!(tr.stop_reason, None);
        assert_eq!(tr.output_tokens, None);
        assert!(!tr.is_silent_failure());
    }

    #[test]
    fn turn_result_different_stop_reason_not_failure() {
        let result = json!({"stopReason": "max_tokens", "usage": {"inputTokens": 10, "outputTokens": 0, "totalTokens": 10}});
        let tr = parse_turn_result(&result);
        assert!(!tr.is_silent_failure());
    }

    #[test]
    fn parse_usage_report_full() {
        let result = json!({
            "success": true,
            "message": "Plan: KIRO POWER | 1 usage breakdowns",
            "data": {
                "planName": "KIRO POWER",
                "billingCycleReset": "2026-08-01",
                "overagesEnabled": true,
                "isEnterprise": false,
                "usageBreakdowns": [{
                    "resourceType": "CREDIT",
                    "displayName": "Credits",
                    "used": 12781.64,
                    "limit": 10000.0,
                    "percentage": 127,
                    "currentOverages": 2781.64,
                    "overageRate": 0.04,
                    "overageCharges": 111.27,
                    "currency": "USD",
                    "hasLimit": true
                }],
                "bonusCredits": [],
                "addOnCredits": [],
                "overageCapable": true
            }
        });
        let report = parse_usage_report(&result).expect("should parse");
        assert_eq!(report.plan_name, "KIRO POWER");
        assert_eq!(report.billing_cycle_reset.as_deref(), Some("2026-08-01"));
        assert_eq!(report.breakdowns.len(), 1);
        let b = &report.breakdowns[0];
        assert_eq!(b.display_name, "Credits");
        assert_eq!(b.used, 12781.64);
        assert_eq!(b.limit, Some(10000.0));
        assert_eq!(b.percentage, Some(127));
        assert_eq!(b.overage_charges, Some(111.27));
        assert_eq!(b.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn parse_usage_report_no_limit_hides_cap() {
        // Pooled enterprise credits: hasLimit=false means the backend's
        // sentinel limit value must not be surfaced.
        let result = json!({
            "success": true,
            "data": {
                "planName": "ENTERPRISE",
                "usageBreakdowns": [{
                    "displayName": "Credits",
                    "used": 320.0,
                    "limit": 999999.0,
                    "hasLimit": false
                }]
            }
        });
        let report = parse_usage_report(&result).expect("should parse");
        assert_eq!(report.breakdowns[0].limit, None);
        assert_eq!(report.billing_cycle_reset, None);
    }

    #[test]
    fn parse_usage_report_missing_has_limit_keeps_numeric_limit() {
        // Regression: exact payload captured live from B0 (kiro-cli in the
        // pr1392 image, 2026-07-13). This kiro-cli version omits `hasLimit`
        // entirely while returning a real numeric `limit` — the old
        // `unwrap_or(false)` default hid the cap and progress bar even though
        // the account was 130% over its 10000-credit limit.
        let result = json!({
            "success": true,
            "message": "Plan: KIRO POWER | 1 usage breakdowns",
            "data": {
                "planName": "KIRO POWER",
                "billingCycleReset": "2026-08-01",
                "overagesEnabled": true,
                "isEnterprise": false,
                "usageBreakdowns": [{
                    "resourceType": "CREDIT",
                    "displayName": "Credits",
                    "used": 13080.52,
                    "limit": 10000.0,
                    "percentage": 130,
                    "currentOverages": 3080.52,
                    "overageRate": 0.04,
                    "overageCharges": 123.220870825752,
                    "currency": "USD"
                }],
                "bonusCredits": []
            }
        });
        let report = parse_usage_report(&result).expect("should parse");
        let b = &report.breakdowns[0];
        assert_eq!(b.limit, Some(10000.0));
        assert_eq!(b.percentage, Some(130));
    }

    #[test]
    fn parse_usage_report_missing_has_limit_and_missing_limit_hides_cap() {
        // No `hasLimit` and no numeric `limit` → still consumption-only.
        let result = json!({
            "success": true,
            "data": {
                "planName": "POOLED",
                "usageBreakdowns": [{"displayName": "Credits", "used": 320.0}]
            }
        });
        let report = parse_usage_report(&result).expect("should parse");
        assert_eq!(report.breakdowns[0].limit, None);
    }

    #[test]
    fn parse_usage_report_failure_returns_none() {
        assert!(parse_usage_report(&json!({"success": false})).is_none());
        assert!(parse_usage_report(&json!({"success": true})).is_none()); // no data
        assert!(parse_usage_report(&json!({})).is_none());
    }

    #[test]
    fn parse_usage_report_empty_breakdowns() {
        let result = json!({
            "success": true,
            "data": {"planName": "FREE", "usageBreakdowns": []}
        });
        let report = parse_usage_report(&result).expect("should parse");
        assert!(report.breakdowns.is_empty());
    }
}

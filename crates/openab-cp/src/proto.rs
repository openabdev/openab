//! Control-plane wire protocol: JSON-RPC 2.0 envelopes and `cp/*` method
//! payloads, following the conventions of `openab-core/src/acp/protocol.rs`.
//!
//! ## Contract summary
//!
//! - Transport: one WebSocket per runtime, text frames, one JSON-RPC message
//!   per frame.
//! - Every request carries `jsonrpc: "2.0"` and a `u64` id; responses echo the
//!   id. Correlation of *delegations* (which span multiple request/response
//!   pairs across two connections) uses `delegation_id`, never the JSON-RPC id.
//! - The first frame on a new connection MUST be `cp/register`. Anything else
//!   is rejected with `NOT_REGISTERED` and the connection is closed.
//! - Delegation ancestry (`chain`) is **CP-constructed**: callers supply only
//!   `parent_delegation_id`; the CP derives the chain from authenticated
//!   identities and its in-flight table. A runtime cannot forge ancestry.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire protocol version. Carried in `cp/register`; the CP rejects
/// registrations with a version it does not support.
pub const PROTOCOL_VERSION: u32 = 1;

// --- JSON-RPC envelopes ---

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
    pub id: u64,
    pub result: Value,
}

impl JsonRpcResponse {
    pub fn new(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub error: ErrorObject,
}

impl JsonRpcErrorResponse {
    pub fn new(id: u64, error: ErrorObject) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            error,
        }
    }
}

/// Incoming message: request, response, or error — distinguished by fields.
#[derive(Debug, Deserialize)]
pub struct JsonRpcMessage {
    pub jsonrpc: Option<String>,
    pub id: Option<u64>,
    pub method: Option<String>,
    pub params: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<ErrorObject>,
}

impl JsonRpcMessage {
    /// Validate this frame as a JSON-RPC 2.0 **request**: the
    /// `jsonrpc` field must be exactly "2.0", and a request id must be
    /// present (all `cp/*` client→CP methods are requests, not
    /// notifications). Returns the request id.
    pub fn require_request_envelope(&self) -> Result<u64, ErrorObject> {
        if self.jsonrpc.as_deref() != Some("2.0") {
            return Err(ErrorObject::new(
                codes::INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        match self.id {
            Some(id) => Ok(id),
            None => Err(ErrorObject::new(
                codes::INVALID_REQUEST,
                "cp/* methods are requests and require an id",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

// --- Error codes (application range, distinct and machine-actionable) ---

pub mod codes {
    /// Frame received before a successful `cp/register` on this connection.
    pub const NOT_REGISTERED: i64 = -32001;
    /// Auth key unknown, or registration claims do not match the identity
    /// bound to the key.
    pub const IDENTITY_MISMATCH: i64 = -32002;
    /// Delegation denied by policy (initiator role, depth, cycle, namespace).
    pub const POLICY_DENIED: i64 = -32003;
    /// No registered, healthy runtime matches the target selector.
    pub const NO_TARGET: i64 = -32004;
    /// Matching targets exist but all are at their advertised capacity.
    /// Explicit fast-fail: the CP never queues (v1 has no durable state).
    pub const SATURATED: i64 = -32005;
    // -32006 is deliberately unassigned. It held a `DEADLINE_EXCEEDED` code
    // that nothing could ever emit: a deadline already in the past is
    // rejected at admission by the policy check (`POLICY_DENIED`, "deadline
    // is in the past"), and a deadline that elapses while the delegation runs
    // is not an error response at all — the sweeper synthesizes a
    // `cp/delegate_result` with status `timeout`. Leaving the slot empty keeps
    // the published numbering stable for anyone who saw the earlier constant.
    /// Serving runtime disconnected while the delegation was in flight.
    pub const TARGET_DISCONNECTED: i64 = -32007;
    /// `delegation_id` already in flight (idempotency guard).
    pub const DUPLICATE_DELEGATION: i64 = -32008;
    /// `cp/register` carried an unsupported protocol version.
    pub const UNSUPPORTED_VERSION: i64 = -32009;
    /// Malformed params for an otherwise valid method.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Invalid JSON-RPC 2.0 envelope (missing/wrong `jsonrpc`, missing id).
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
}

// --- cp/register ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Primary,
    Worker,
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentType::Primary => write!(f, "primary"),
            AgentType::Worker => write!(f, "worker"),
        }
    }
}

/// Params of `cp/register`, the mandatory first frame.
///
/// `namespace`, `name`, and `agent_type` are **assertions to be verified**,
/// not authorization inputs: the CP compares them against the immutable
/// claims bound to the presented auth key and rejects any mismatch with
/// `IDENTITY_MISMATCH`. They exist in the frame so a misconfigured runtime
/// fails loudly at registration instead of being silently re-identified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterParams {
    pub protocol_version: u32,
    pub namespace: String,
    pub name: String,
    #[serde(rename = "type")]
    pub agent_type: AgentType,
    /// Runtime-generated per-process id; distinguishes replicas of the same
    /// logical agent during rolling deploys.
    pub instance_id: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// Advertised concurrency budget. The CP may clamp this to a
    /// config-defined cap for the identity.
    #[serde(default = "default_max_sessions")]
    pub max_delegated_sessions: u32,
}

fn default_max_sessions() -> u32 {
    1
}

/// Result of a successful `cp/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAck {
    pub protocol_version: u32,
    /// Interval at which the runtime must send `cp/heartbeat`.
    pub heartbeat_interval_secs: u64,
    /// Lease duration; missing heartbeats past this window deregisters the
    /// instance and fails its in-flight delegations.
    pub lease_expiry_secs: u64,
    /// The effective (possibly clamped) concurrency budget.
    pub effective_max_delegated_sessions: u32,
}

// --- cp/heartbeat ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatParams {
    pub instance_id: String,
    /// Current number of delegated sessions the runtime is serving; lets the
    /// CP correct drift in its own in-flight accounting.
    #[serde(default)]
    pub active_delegated_sessions: u32,
}

// --- cp/delegate ---

/// Target selector: exact logical name, or label match (all pairs must match).
/// Exactly one of the two must be set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TargetSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::BTreeMap<String, String>>,
}

/// Params of `cp/delegate` as sent by the initiating runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateParams {
    /// Caller-generated unique id (idempotency key). The CP rejects a second
    /// in-flight delegation with the same id.
    pub delegation_id: String,
    pub target: TargetSelector,
    pub prompt: String,
    /// Absolute RFC3339 deadline. Mandatory: the CP rejects missing, past, or
    /// over-cap deadlines. A child deadline can never exceed the parent's
    /// remaining budget.
    pub deadline: chrono::DateTime<chrono::Utc>,
    /// If this delegation is issued while serving another delegation, the id
    /// of that parent. The CP derives the ancestry chain from this — the
    /// chain is never client-supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_delegation_id: Option<String>,
}

/// Params of `cp/delegate` as forwarded to the serving runtime. The CP stamps
/// the authenticated origin and the CP-constructed chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateForward {
    pub delegation_id: String,
    pub prompt: String,
    pub deadline: chrono::DateTime<chrono::Utc>,
    /// Authenticated identity of the initiating agent (`namespace/name`).
    pub from: String,
    /// CP-constructed delegation ancestry, root first. The serving runtime
    /// can trust every element: each hop was authenticated by the CP.
    pub chain: Vec<String>,
}

/// Immediate result of `cp/delegate` (routing acceptance, not completion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateAck {
    pub delegation_id: String,
    /// The chosen serving instance's logical name (`namespace/name`).
    pub assigned_to: String,
}

// --- cp/delegate_result ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    Completed,
    Failed,
    Timeout,
    Cancelled,
    TargetDisconnected,
}

/// Params of `cp/delegate_result` — emitted by the serving **runtime** when
/// the agent's turn ends (protocol-mandatory; never depends on the model),
/// or synthesized by the CP on timeout/disconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateResultParams {
    pub delegation_id: String,
    pub status: DelegationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --- cp/cancel ---

/// Params of `cp/cancel`: from the initiator to abort an in-flight
/// delegation, or from the CP to the serving runtime (best effort) after a
/// timeout or initiator cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelParams {
    pub delegation_id: String,
    pub reason: String,
}

// --- method names ---

pub mod methods {
    pub const REGISTER: &str = "cp/register";
    pub const HEARTBEAT: &str = "cp/heartbeat";
    pub const DELEGATE: &str = "cp/delegate";
    pub const DELEGATE_RESULT: &str = "cp/delegate_result";
    pub const CANCEL: &str = "cp/cancel";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_params_roundtrip_with_type_rename() {
        let json = serde_json::json!({
            "protocol_version": 1,
            "namespace": "prod",
            "name": "koudu",
            "type": "primary",
            "instance_id": "i-abc",
            "labels": {"backend": "kiro"},
            "max_delegated_sessions": 4
        });
        let p: RegisterParams = serde_json::from_value(json).unwrap();
        assert_eq!(p.agent_type, AgentType::Primary);
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["type"], "primary");
    }

    #[test]
    fn register_defaults_apply() {
        let json = serde_json::json!({
            "protocol_version": 1,
            "namespace": "prod",
            "name": "w1",
            "type": "worker",
            "instance_id": "i-1"
        });
        let p: RegisterParams = serde_json::from_value(json).unwrap();
        assert!(p.labels.is_empty());
        assert_eq!(p.max_delegated_sessions, 1);
    }

    #[test]
    fn delegate_params_require_deadline() {
        let json = serde_json::json!({
            "delegation_id": "d-1",
            "target": {"name": "w1"},
            "prompt": "hi"
        });
        assert!(serde_json::from_value::<DelegateParams>(json).is_err());
    }

    #[test]
    fn delegation_status_snake_case() {
        assert_eq!(
            serde_json::to_value(DelegationStatus::TargetDisconnected).unwrap(),
            serde_json::json!("target_disconnected")
        );
    }

    #[test]
    fn incoming_message_distinguishes_request_and_response() {
        let req: JsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"cp/heartbeat","params":{}}"#)
                .unwrap();
        assert!(req.method.is_some() && req.result.is_none());
        let resp: JsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap();
        assert!(resp.method.is_none() && resp.result.is_some());
    }

    #[test]
    fn request_envelope_validation() {
        let ok: JsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"cp/heartbeat"}"#).unwrap();
        assert_eq!(ok.require_request_envelope().unwrap(), 7);

        // Missing jsonrpc.
        let no_ver: JsonRpcMessage =
            serde_json::from_str(r#"{"id":7,"method":"cp/heartbeat"}"#).unwrap();
        assert_eq!(
            no_ver.require_request_envelope().unwrap_err().code,
            codes::INVALID_REQUEST
        );

        // Wrong version.
        let bad_ver: JsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"1.0","id":7,"method":"cp/heartbeat"}"#).unwrap();
        assert_eq!(
            bad_ver.require_request_envelope().unwrap_err().code,
            codes::INVALID_REQUEST
        );

        // Notification shape (no id).
        let no_id: JsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"cp/heartbeat"}"#).unwrap();
        assert_eq!(
            no_id.require_request_envelope().unwrap_err().code,
            codes::INVALID_REQUEST
        );
    }
}

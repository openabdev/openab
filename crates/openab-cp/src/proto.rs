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
//! - `delegation_id` is client-supplied and REUSABLE (cancel-then-retry), so
//!   correlation of one *admission* of that id — which terminal frame belongs
//!   to which routing decision — uses the CP-minted
//!   [`AdmissionToken`]: carried on the `cp/delegate` ack and forwarded frame,
//!   echoed by the serving runtime in `cp/delegate_result` (required), carried
//!   on `cp/cancel` in both directions (required), carried on `cp/delegate` as
//!   `parent_admission` whenever the request names a parent (required), and
//!   stamped on every initiator-bound terminal frame. Every surface that
//!   references an *existing* delegation names it by the (id, admission) pair;
//!   a bare reusable id is never accepted as a reference.
//! - The first frame on a new connection MUST be `cp/register`. Anything else
//!   is rejected with `NOT_REGISTERED` and the connection is closed.
//! - Delegation ancestry (`chain`) is **CP-constructed**: callers never supply
//!   a chain. They supply a parent *reference*, and that reference is the
//!   coupled pair `parent_delegation_id` + `parent_admission` — a parented
//!   `cp/delegate` MUST carry both (an id without a token is
//!   `INVALID_PARAMS`), so the parent is named by a specific admission rather
//!   than by a reusable id. The CP derives the chain, depth, cycle and
//!   deadline-budget inputs from the entry that reference resolves to, using
//!   authenticated identities and its in-flight table. A root delegation
//!   carries neither field. A runtime cannot forge ancestry, nor inherit the
//!   chain and budget of an admission it was not serving.

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
///
/// The parent reference is a **pair**: `parent_delegation_id` names the
/// delegation, `parent_admission` names which admission of it (see
/// [`AdmissionToken`]). Both are `Option` on the struct because a root
/// delegation carries neither — that shape is unchanged on the wire — but the
/// CP enforces their coupling at admission: an id without a token is
/// `INVALID_PARAMS`, never "whatever admission holds that id now". An optional
/// token on a parented request would be exactly the wildcard that lets stale
/// work inherit a live re-admission's chain and deadline budget.
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
    ///
    /// Present ⇒ `parent_admission` MUST also be present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_delegation_id: Option<String>,
    /// Which admission of `parent_delegation_id` this delegation is a child
    /// of — the token the serving runtime learned from
    /// [`DelegateForward::admission`] for the parent it is currently serving.
    ///
    /// Required whenever `parent_delegation_id` is present, and meaningless
    /// without it (the CP refuses a token-without-id as malformed rather than
    /// ignoring it, so a client bug surfaces instead of silently producing a
    /// root delegation). `delegation_id` alone names a *slot* that
    /// cancel-then-retry legitimately reuses; the parent lookup feeds the
    /// CP's own policy inputs — ancestry chain, depth, cycle, and the
    /// deadline budget a child is clamped to — so it must name the admission
    /// those inputs were computed for. Without the token, a residual task
    /// from a parent admission A that has already ended could submit a child
    /// against the same id after it was re-admitted as B to the same worker,
    /// and the child would inherit B's trusted chain and B's remaining
    /// budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_admission: Option<AdmissionToken>,
}

/// Params of `cp/delegate` as forwarded to the serving runtime. The CP stamps
/// the authenticated origin and the CP-constructed chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateForward {
    pub delegation_id: String,
    /// The CP-minted admission token for THIS admission of
    /// `delegation_id` — see [`AdmissionToken`]. The serving runtime MUST
    /// echo it in the matching `cp/delegate_result`.
    pub admission: AdmissionToken,
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
    /// The CP-minted admission token for this admission — see
    /// [`AdmissionToken`]. The initiator correlates terminal frames on it,
    /// because `delegation_id` alone is reusable.
    pub admission: AdmissionToken,
    /// The chosen serving instance's logical name (`namespace/name`).
    pub assigned_to: String,
}

/// Protocol-visible identity of ONE admission of a `delegation_id`.
///
/// `delegation_id` is client-supplied and deliberately reusable:
/// cancel-then-retry is an ordinary client pattern, and with a single replica
/// the retry routes to the same worker, so `(namespace, delegation_id)` +
/// serving instance is not a stable identity over time. Every frame that
/// belongs to a specific admission therefore carries this token:
///
/// - `cp/delegate` ack → the initiator learns it;
/// - forwarded `cp/delegate` → the serving runtime learns it;
/// - `cp/delegate_result` → the serving runtime MUST echo it (required field);
///   the CP drops a result whose token is not the live admission's, so a late
///   result for a cancelled admission can never be delivered as, or commit,
///   the delegation that reused the id;
/// - every initiator-bound terminal frame, CP-synthesized `timeout` and
///   `target_disconnected` included, carries the token of the admission it
///   ends, so "first terminal frame wins" is keyed per admission rather than
///   per reusable id;
/// - `cp/cancel` carries it in BOTH directions (required field). From the
///   initiator it names the admission to abort, so a retried cancel cannot
///   remove the re-admission that replaced its target; on every CP-synthesized
///   cancel the CP stamps the token of the admission it is ending, so a
///   best-effort cancel that overtakes a same-id re-admission's forward is
///   identifiable at the worker as belonging to the admission that is already
///   over.
/// - the parent reference on `cp/delegate` carries it
///   (`parent_admission`, required whenever `parent_delegation_id` is
///   present). The parent lookup is what lets a child inherit a
///   CP-constructed chain and a parent's remaining deadline budget, so it
///   must name the admission those were derived for: after a parent
///   admission ends and its id is re-admitted, a child composed against the
///   ended admission is refused instead of adopting the live one's policy
///   envelope.
///
/// The value is a per-namespace monotonic counter. Namespace-scoped on
/// purpose: a single global counter placed on the wire would disclose
/// cross-namespace delegation volume, the class of oracle the namespace-scoped
/// in-flight key exists to remove. Within a namespace the number is no more
/// than what `cp/list_agents` already shows that namespace about itself.
pub type AdmissionToken = u64;

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
///
/// `admission` is REQUIRED, not optional: it is the only thing that ties the
/// frame to one admission of a reusable `delegation_id`. A missing token is a
/// malformed frame (`INVALID_PARAMS`), never a wildcard — an optional token
/// would leave exactly the misdelivery path it exists to close. The wire is
/// pre-1.0 and every serving runtime in this stack learns the token from the
/// forwarded `cp/delegate`, so echoing it costs nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateResultParams {
    pub delegation_id: String,
    /// Echo of [`DelegateForward::admission`]. On CP-synthesized terminal
    /// frames the CP fills in the token of the admission it is ending.
    pub admission: AdmissionToken,
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
///
/// `admission` is REQUIRED for the same reason it is required on
/// [`DelegateResultParams`], and the asymmetry would have been the hole:
/// `delegation_id` alone names a *slot* that cancel-then-retry legitimately
/// reuses, not the work being aborted. From the initiator the token is the
/// abort target, so a retried cancel is refused instead of removing the
/// re-admission that replaced its target. On CP-synthesized cancels the CP
/// stamps the token of the admission it is ending, so a best-effort cancel for
/// an expired admission that reaches the worker *after* a same-id
/// re-admission's forward names the finished admission rather than the live
/// one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelParams {
    pub delegation_id: String,
    /// The admission being cancelled — see [`AdmissionToken`]. Required: a
    /// missing token is a malformed frame (`INVALID_PARAMS`), never a
    /// "cancel whatever holds this id now" wildcard.
    pub admission: AdmissionToken,
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

    /// Every ` ```json ` block in the ADR, in document order.
    ///
    /// `include_str!` is a compile-time read: the examples become part of this
    /// crate's source, so the check costs no runtime filesystem access and
    /// obeys the crate's no-fs-in-unit-tests rule. It does couple the test to
    /// the repository layout — deliberately. `openab-cp` is a workspace binary
    /// crate, never vendored on its own, and a fixture copied into this file
    /// would drift from the ADR exactly the way the ADR drifted from the
    /// structs.
    fn adr_json_examples() -> Vec<Value> {
        const ADR: &str = include_str!("../../../docs/adr/agent-control-plane.md");
        let mut out = Vec::new();
        let mut open: Option<String> = None;
        for line in ADR.lines() {
            let line = line.trim_end();
            match (&mut open, line) {
                (None, "```json") => open = Some(String::new()),
                (Some(_), "```") => {
                    let body = open.take().expect("block is open");
                    out.push(serde_json::from_str(&body).unwrap_or_else(|e| {
                        panic!("an ADR ```json block is not valid JSON: {e}\n{body}")
                    }));
                }
                (Some(buf), l) => {
                    buf.push_str(l);
                    buf.push('\n');
                }
                (None, _) => {}
            }
        }
        assert!(open.is_none(), "unterminated ```json block in the ADR");
        out
    }

    fn sorted_keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v
            .as_object()
            .unwrap_or_else(|| panic!("expected a JSON object, got {v}"))
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    }

    #[test]
    fn adr_wire_examples_round_trip_through_the_serde_structs() {
        // `docs/adr/agent-control-plane.md` is the authoritative wire contract
        // and its examples are what a client implementer copies. They drifted
        // once: the `cp/delegate` example showed a client-sent `chain`, a field
        // `DelegateParams` has never had, and serde silently ignored it, so
        // nothing failed. This test makes the examples part of the compiled
        // contract — each block must parse into the struct its `method` names,
        // and the round trip must reproduce the exact key set, so a field the
        // example has and the struct does not (or the reverse) fails here.
        //
        // A new ADR example with a shape this dispatch does not recognize
        // fails rather than being skipped: extend both together.
        let examples = adr_json_examples();
        assert!(
            examples.len() >= 4,
            "expected the delegate, forwarded-delegate, result, and cancel \
             examples; found {}",
            examples.len()
        );
        for ex in &examples {
            let method = ex["method"]
                .as_str()
                .unwrap_or_else(|| panic!("ADR example carries no method: {ex}"));
            let params = &ex["params"];
            // `from` is stamped by the CP and appears only on the forwarded
            // frame, so it distinguishes the two `cp/delegate` shapes.
            let forwarded = params.get("from").is_some();
            let round_tripped = match (method, forwarded) {
                (methods::DELEGATE, false) => {
                    // The finding, pinned: what an initiator sends carries
                    // neither the CP-constructed chain nor the CP-minted token.
                    assert!(
                        params.get("chain").is_none(),
                        "the initiator-sent cp/delegate example must not show a \
                         client-supplied `chain` — the CP constructs it"
                    );
                    assert!(
                        params.get("admission").is_none(),
                        "the initiator-sent cp/delegate example must not show an \
                         `admission` — the CP mints it and returns it in the ack"
                    );
                    // The parent reference is a pair, and key-set equality
                    // alone cannot catch a missing half: an example with
                    // `parent_delegation_id` and no `parent_admission` parses
                    // into `None`, which is then skipped on serialization, so
                    // the key sets would still match. Assert the coupling
                    // directly — an example showing a bare parent id would
                    // document exactly the wildcard the CP refuses, and it is
                    // what a client implementer copies.
                    assert_eq!(
                        params.get("parent_delegation_id").is_some(),
                        params.get("parent_admission").is_some(),
                        "the cp/delegate example must show `parent_delegation_id` \
                         and `parent_admission` together, or neither (root)"
                    );
                    let p: DelegateParams = serde_json::from_value(params.clone())
                        .expect("cp/delegate example must parse as DelegateParams");
                    serde_json::to_value(&p).expect("serializable")
                }
                (methods::DELEGATE, true) => {
                    let p: DelegateForward = serde_json::from_value(params.clone())
                        .expect("forwarded cp/delegate example must parse as DelegateForward");
                    serde_json::to_value(&p).expect("serializable")
                }
                (methods::DELEGATE_RESULT, _) => {
                    let p: DelegateResultParams = serde_json::from_value(params.clone())
                        .expect("cp/delegate_result example must parse as DelegateResultParams");
                    serde_json::to_value(&p).expect("serializable")
                }
                (methods::CANCEL, _) => {
                    let p: CancelParams = serde_json::from_value(params.clone())
                        .expect("cp/cancel example must parse as CancelParams");
                    serde_json::to_value(&p).expect("serializable")
                }
                (m, _) => panic!("the ADR documents a `{m}` example this test does not check"),
            };
            assert_eq!(
                sorted_keys(params),
                sorted_keys(&round_tripped),
                "the `{method}` example in the ADR has drifted from its struct"
            );
        }
    }

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
    fn parent_reference_is_a_pair_and_root_delegations_are_unchanged_on_the_wire() {
        // A root delegation carries neither field, and neither appears when it
        // is serialized — the pre-token root shape is byte-compatible, which is
        // why the wire break is confined to parented requests.
        let root = serde_json::json!({
            "delegation_id": "d-1",
            "target": {"name": "w1"},
            "prompt": "hi",
            "deadline": "2026-08-06T22:45:00Z"
        });
        let p: DelegateParams = serde_json::from_value(root).unwrap();
        assert!(p.parent_delegation_id.is_none());
        assert!(p.parent_admission.is_none());
        let back = serde_json::to_value(&p).unwrap();
        assert!(back.get("parent_delegation_id").is_none());
        assert!(
            back.get("parent_admission").is_none(),
            "a root delegation must not put an empty parent reference on the wire"
        );

        // A parented request carries both halves, and both round-trip.
        let parented = serde_json::json!({
            "delegation_id": "d-2",
            "target": {"name": "w1"},
            "prompt": "hi",
            "deadline": "2026-08-06T22:45:00Z",
            "parent_delegation_id": "d-1",
            "parent_admission": 42
        });
        let p: DelegateParams = serde_json::from_value(parented).unwrap();
        assert_eq!(p.parent_delegation_id.as_deref(), Some("d-1"));
        assert_eq!(p.parent_admission, Some(42));
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["parent_admission"], 42);
        // Deserialization deliberately accepts a half-filled pair: the coupling
        // is a CP admission rule (`INVALID_PARAMS`, with a message naming the
        // missing half) rather than a serde error, so the refusal is a protocol
        // error the caller can act on instead of a parse failure.
        let half = serde_json::json!({
            "delegation_id": "d-3",
            "target": {"name": "w1"},
            "prompt": "hi",
            "deadline": "2026-08-06T22:45:00Z",
            "parent_delegation_id": "d-1"
        });
        let p: DelegateParams = serde_json::from_value(half).unwrap();
        assert!(p.parent_admission.is_none());
    }

    #[test]
    fn delegate_result_requires_the_admission_token() {
        // The token is the only thing tying a result frame to ONE admission of
        // a reusable delegation_id. A frame without it must be malformed, not
        // a wildcard that matches whatever admission is live.
        let without = serde_json::json!({
            "delegation_id": "d-1",
            "status": "completed",
            "result": "done"
        });
        assert!(serde_json::from_value::<DelegateResultParams>(without).is_err());

        let with = serde_json::json!({
            "delegation_id": "d-1",
            "admission": 7,
            "status": "completed",
            "result": "done"
        });
        let p: DelegateResultParams = serde_json::from_value(with).unwrap();
        assert_eq!(p.admission, 7);
        // And it round-trips onto the wire (initiator-bound frames carry it).
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["admission"], 7);
    }

    #[test]
    fn ack_and_forward_carry_the_admission_token() {
        let ack = DelegateAck {
            delegation_id: "d-1".into(),
            admission: 3,
            assigned_to: "prod/worker-1".into(),
        };
        assert_eq!(serde_json::to_value(&ack).unwrap()["admission"], 3);
        let fwd = DelegateForward {
            delegation_id: "d-1".into(),
            admission: 3,
            prompt: "hi".into(),
            deadline: chrono::Utc::now(),
            from: "prod/koudu".into(),
            chain: vec!["prod/koudu".into()],
        };
        assert_eq!(serde_json::to_value(&fwd).unwrap()["admission"], 3);
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

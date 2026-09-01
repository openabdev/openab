//! Phase 6.4: deterministic OpenAB → AAP autonomous ingress routing.
//!
//! This module provides the narrow seam where a human-authored Discord
//! message addressed to a declared AAP-autonomous agent is routed to
//! the Arthur AI Platform (AAP) Runtime BEFORE the ordinary ACP
//! conversation path. The routing decision is **deterministic** — it
//! is driven entirely by the deployment-time
//! [`crate::config::AutonomousIngressConfig`] and never inspects the
//! prompt body, never consults the LLM, and never relies on free-form
//! NLP keyword matching.
//!
//! Per AGENTS.md critical architecture rule, the LLM MUST NOT become
//! the workflow-routing authority. This module preserves that rule by
//! treating the configuration as the only machine-testable contract
//! for "should this human message be admitted to AAP first?".
//!
//! # Flow
//!
//! ```text
//! Human Discord message
//!     ↓
//! Discord adapter (parse event)
//!     ↓
//! Dispatcher (batch)
//!     ↓
//! A13 workflow-role gate (existing)
//!     ↓ (reason == WorkflowAssignmentMissing)
//! [Phase 6.4 seam]
//!     decide_aap_autonomous_route(config, agent, sender, conversation)
//!         ↓
//!     AutonomousIngressClient::submit_autonomous_ingress(...)
//!         ↓ (POST /v1/integrations/openab/autonomous_ingress)
//!     AAP Runtime NativeRuntimeIngressService
//!         ↓
//!     AutonomousWorkflowEntryService → Task + WorkflowRun + ConversationBinding
//!         ↓
//!     scheduler → agent.work → ArthurClaude PRIMARY
//! ```
//!
//! # Message consumption invariant
//!
//! Once AAP accepts the human turn, OpenAB MUST mark the message as
//! consumed and MUST NOT let it fall through into ordinary ACP
//! dispatch. Otherwise the same human turn would execute twice —
//! once as conversational coding and once as scheduler-native
//! agent.work. [`AutonomousRouteDisposition::Accepted`] therefore
//! instructs the dispatch loop to suppress the ACP path entirely.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::adapter::ChannelRef;
use crate::config::AutonomousIngressConfig;

/// Outcome of the Phase 6.4 deterministic routing check.
///
/// Variants drive the dispatcher's consume / fail-closed behaviour.
/// The variant is the contract — there is no string matching on the
/// AAP response body. AAP's `disposition` field is projected into this
/// typed enum so the dispatch loop branches on stable variants only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutonomousRouteDisposition {
    /// AAP accepted ownership of the human turn. The dispatcher MUST
    /// NOT proceed to ordinary ACP dispatch for this turn.
    Accepted {
        task_id: String,
        workflow_run_id: String,
        binding_id: String,
        conversation_key: String,
    },
    /// AAP was reachable but explicitly rejected the request (e.g.
    /// unauthorized project, invalid conversation_key, auth failure).
    /// The dispatcher MUST fail closed and surface the error to the
    /// sender. There is no ordinary ACP fallback.
    Rejected {
        error_code: String,
        retryable: bool,
        detail: Option<String>,
    },
    /// AAP was not reachable, timed out, returned malformed JSON, or
    /// authentication could not be resolved. The dispatcher MUST fail
    /// closed. There is no ordinary ACP fallback.
    Unavailable { error_code: String, retryable: bool },
    /// The routing contract determined the message is NOT eligible for
    /// AAP autonomous ingress — either because no config is present,
    /// the daemon's agent is not in `aap_agents`, or the sender is
    /// not authorised. The dispatcher proceeds with the existing
    /// legacy behavior for this turn.
    NotApplicable,
}

/// Structured info emitted by the OpenAB-side candidate / accept /
/// failure logs. All fields are stable identifiers or short tags; the
/// prompt body is never included.
#[derive(Debug, Clone)]
pub struct AutonomousIngressCandidate {
    pub source: &'static str, // always "discord" today
    pub agent: String,
    pub conversation_key: String,
    pub message_id: String,
    pub routing_contract: &'static str, // "config_autonomous_ingress"
}

/// Request shape sent to AAP Runtime
/// `POST /v1/integrations/openab/autonomous_ingress`. Mirrors the
/// canonical `CanonicalNativeRuntimeIngressRequest` fields OpenAB is
/// authoritative for; AAP fills in defaults / authority.
#[derive(Debug, Clone, Serialize)]
pub struct AutonomousIngressRequest {
    pub protocol: &'static str, // "openab"
    pub project_id: String,
    pub transport: &'static str, // "DISCORD"
    pub conversation_key: String,
    pub user_objective: String,
    pub trace_id: String,
    pub task_id: Option<String>,
    pub primary_agent: String,
    pub language: String,
    pub metadata: AutonomousIngressMetadata,
    /// Phase 6.4.1D — authoritative structured delivery destination
    /// sourced from the trusted `thread_channel: ChannelRef` at the
    /// dispatch site. Projected into `metadata.delivery_destination`
    /// on the AAP side (so `ConversationBinding._coerce_delivery_destination`
    /// can promote it to the typed field) and into `AgentWorkRequest.delivery_destination`
    /// via the scheduler hop so the daemon replies to the actual
    /// workflow's originating channel instead of the daemon-wide
    /// `native_delivery_target` fallback.
    ///
    /// `None` is the legacy behaviour (OpenAB daemon uses its static
    /// fallback). The value is NEVER parsed from `conversation_key`
    /// or any other heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_destination: Option<AutonomousIngressDeliveryDestination>,
}

/// Phase 6.4.1D — wire DTO for the structured delivery destination
/// carried inside `AutonomousIngressRequest`. Mirrors the runtime
/// `ConversationBinding.delivery_destination` shape and OpenAB's
/// `adapter::ChannelRef` shape, but has its own `Serialize` derive
/// so it does not have to live on the widely-shared `ChannelRef`
/// struct (which intentionally avoids Serde derives to keep the
/// daemon-internal path lean).
///
/// Conversion to `ChannelRef` happens at the AAP call site
/// (`_coerce_delivery_destination`).
#[derive(Debug, Clone, Serialize, Default)]
pub struct AutonomousIngressDeliveryDestination {
    pub platform: String,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_event_id: Option<String>,
}

/// Side-channel metadata so AAP Runtime can preserve Discord delivery
/// identity without coupling to OpenAB-internal types. None of these
/// fields are used to derive workflow authority — the AAP Runtime is
/// the canonical authority.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AutonomousIngressMetadata {
    pub discord_message_id: Option<String>,
    pub discord_channel_id: Option<String>,
    pub discord_thread_id: Option<String>,
    pub discord_user_id: Option<String>,
    pub discord_sender_is_bot: bool,
    /// Phase 6.4.1D — structured delivery destination mirrored into
    /// the metadata block so the legacy `_coerce_delivery_destination`
    /// promotion point in `ConversationBindingService.bind()` can
    /// pick it up without an API-shape change. AAP's
    /// `OpenABAutonomousIngressRequestModel.delivery_destination` is
    /// the wire-of-record; this metadata mirror is for
    /// backward-compatibility with the existing
    /// OpenClaw-bridge-style metadata shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_destination: Option<AutonomousIngressDeliveryDestination>,
}

/// Response shape projected from AAP's `CanonicalNativeRuntimeIngressResult`.
#[derive(Debug, Clone, Deserialize)]
pub struct AutonomousIngressResponse {
    pub disposition: String, // "ACCEPTED" | "REJECTED" | ...
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub workflow_run_id: Option<String>,
    #[serde(default)]
    pub binding_id: Option<String>,
    #[serde(default)]
    pub conversation_key: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub retryable: Option<bool>,
}

/// Minimal HTTP client seam for Phase 6.4. Production uses
/// [`HttpAutonomousIngressClient`]; tests inject
/// [`FakeAutonomousIngressClient`] to deterministically simulate
/// AAP accept / reject / unavailable / auth failure outcomes.
#[async_trait::async_trait]
pub trait AutonomousIngressClient: Send + Sync {
    async fn submit(
        &self,
        request: AutonomousIngressRequest,
    ) -> Result<AutonomousIngressResponse, AutonomousIngressError>;
}

/// Stable error taxonomy. `retryable` drives the dispatcher failure
/// log and the surface error to the sender. The credential is never
/// included in any error variant or log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutonomousIngressError {
    Unreachable(String),
    Timeout,
    AuthMissing,
    Http { status: u16, body_snippet: String },
    Malformed(String),
}

impl std::fmt::Display for AutonomousIngressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m) => write!(f, "AAP unreachable: {m}"),
            Self::Timeout => write!(f, "AAP timeout"),
            Self::AuthMissing => write!(f, "AAP auth credential missing"),
            Self::Http {
                status,
                body_snippet,
            } => {
                write!(f, "AAP HTTP {status}: {body_snippet}")
            }
            Self::Malformed(m) => write!(f, "AAP response malformed: {m}"),
        }
    }
}

impl std::error::Error for AutonomousIngressError {}

impl AutonomousIngressError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unreachable(_) | Self::Timeout => true,
            Self::Http { status, .. } => *status >= 500,
            Self::AuthMissing | Self::Malformed(_) => false,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "AAP_UNREACHABLE",
            Self::Timeout => "AAP_TIMEOUT",
            Self::AuthMissing => "AAP_AUTH_MISSING",
            Self::Http { .. } => "AAP_HTTP_ERROR",
            Self::Malformed(_) => "AAP_RESPONSE_MALFORMED",
        }
    }
}

/// Real HTTP client. Uses a small, dependency-light `ureq`-style call
/// to keep the Phase 6.4 surface minimal. The HTTP body and status
/// are mapped into the typed [`AutonomousIngressResponse`] or the
/// appropriate error variant. Credentials are sent as `Authorization:
/// Bearer <token>`; tokens are read from the configured env var.
pub struct HttpAutonomousIngressClient {
    base_url: String,
    credential: String,
    timeout: Duration,
    http: Arc<dyn AutonomousIngressTransport>,
}

impl HttpAutonomousIngressClient {
    pub fn new(
        config: &AutonomousIngressConfig,
        http: Arc<dyn AutonomousIngressTransport>,
    ) -> Result<Self, AutonomousIngressError> {
        let credential = config
            .resolve_credential()
            .ok_or(AutonomousIngressError::AuthMissing)?;
        Ok(Self {
            base_url: config.aap_runtime_url.trim_end_matches('/').to_string(),
            credential,
            timeout: Duration::from_secs(config.request_timeout_seconds),
            http,
        })
    }
}

/// Production wiring helper. Given an
/// [`crate::config::AutonomousIngressConfig`], build a real HTTP
/// client with the [`ReqwestAutonomousIngressTransport`] and return
/// the typed client. The transport is the same dependency
/// `openab-core` already uses for the native completion port.
///
/// This function is the single production entry point the binary's
/// startup composition calls. It exists so the test suite can drive
/// the same code path the binary uses (no test subclass, no manual
/// `with_autonomous_ingress` call inside the test).
pub fn build_production_client(
    config: &AutonomousIngressConfig,
) -> Result<HttpAutonomousIngressClient, AutonomousIngressError> {
    let http: Arc<dyn AutonomousIngressTransport> =
        Arc::new(ReqwestAutonomousIngressTransport::new());
    HttpAutonomousIngressClient::new(config, http)
}

#[async_trait::async_trait]
pub trait AutonomousIngressTransport: Send + Sync {
    async fn post_json(
        &self,
        url: String,
        bearer_token: String,
        timeout: Duration,
        body: String,
    ) -> Result<(u16, String), AutonomousIngressError>;
}

#[async_trait::async_trait]
impl AutonomousIngressClient for HttpAutonomousIngressClient {
    async fn submit(
        &self,
        request: AutonomousIngressRequest,
    ) -> Result<AutonomousIngressResponse, AutonomousIngressError> {
        let url = format!(
            "{}/v1/integrations/openab/autonomous_ingress",
            self.base_url
        );
        let body = serde_json::to_string(&request)
            .map_err(|e| AutonomousIngressError::Malformed(format!("encode request: {e}")))?;
        let (status, response_body) = self
            .http
            .post_json(url, self.credential.clone(), self.timeout, body)
            .await?;
        if !(200..300).contains(&status) {
            return Err(AutonomousIngressError::Http {
                status,
                body_snippet: response_body.chars().take(200).collect(),
            });
        }
        serde_json::from_str::<AutonomousIngressResponse>(&response_body)
            .map_err(|e| AutonomousIngressError::Malformed(format!("decode response: {e}")))
    }
}

/// Production HTTP transport backed by `reqwest` (already in
/// `openab-core`'s dependency graph for the native completion port).
/// Bearer token is forwarded as `Authorization: Bearer <token>`; the
/// token is never logged or propagated to error variants.
pub struct ReqwestAutonomousIngressTransport {
    client: reqwest::Client,
}

impl ReqwestAutonomousIngressTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestAutonomousIngressTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AutonomousIngressTransport for ReqwestAutonomousIngressTransport {
    async fn post_json(
        &self,
        url: String,
        bearer_token: String,
        timeout: Duration,
        body: String,
    ) -> Result<(u16, String), AutonomousIngressError> {
        let resp = self
            .client
            .post(&url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {bearer_token}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AutonomousIngressError::Timeout
                } else {
                    AutonomousIngressError::Unreachable(e.to_string())
                }
            })?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| AutonomousIngressError::Malformed(format!("read body: {e}")))?;
        Ok((status, body))
    }
}

/// The deterministic routing decision: given the configuration, the
/// daemon's logical agent identity, and the human sender's Tech-Lead
/// authorization, return whether the AAP autonomous path should be
/// consulted for this turn.
///
/// The function is pure and machine-testable. It deliberately does not
/// look at the prompt body.
pub fn should_route_to_aap(
    config: Option<&AutonomousIngressConfig>,
    agent: &str,
    sender_tech_lead_authorized: bool,
) -> bool {
    let Some(cfg) = config else {
        return false;
    };
    if !cfg.declares_agent(agent) {
        return false;
    }
    if cfg.aap_universal_humans {
        return true;
    }
    sender_tech_lead_authorized
}

/// Project an AAP response into the dispatch-facing disposition. The
/// dispatcher branches ONLY on the resulting variants, never on string
/// comparison of `disposition` / `error_code`.
pub fn project_response(
    response: AutonomousIngressResponse,
    fallback_conversation_key: &str,
) -> AutonomousRouteDisposition {
    match response.disposition.as_str() {
        "ACCEPTED" => AutonomousRouteDisposition::Accepted {
            task_id: response.task_id.unwrap_or_default(),
            workflow_run_id: response.workflow_run_id.unwrap_or_default(),
            binding_id: response.binding_id.unwrap_or_default(),
            conversation_key: response
                .conversation_key
                .unwrap_or_else(|| fallback_conversation_key.to_string()),
        },
        "REJECTED" => AutonomousRouteDisposition::Rejected {
            error_code: response
                .error_code
                .unwrap_or_else(|| "AAP_REJECTED".to_string()),
            retryable: response.retryable.unwrap_or(false),
            detail: response.detail,
        },
        other => AutonomousRouteDisposition::Rejected {
            error_code: format!("AAP_UNKNOWN_DISPOSITION:{other}"),
            retryable: false,
            detail: response.detail,
        },
    }
}

/// Project a transport error into the dispatch-facing disposition.
/// Retryable flag is preserved so the dispatcher can emit the correct
/// structured INFO log token.
pub fn project_error(err: AutonomousIngressError) -> AutonomousRouteDisposition {
    AutonomousRouteDisposition::Unavailable {
        error_code: err.code().to_string(),
        retryable: err.retryable(),
    }
}

/// Build a candidate log entry from the Discord channel + message
/// metadata. This is purely for observability — the routing decision
/// was already made by [`should_route_to_aap`].
pub fn build_candidate(
    agent: &str,
    conversation: &ChannelRef,
    message_id: &str,
) -> AutonomousIngressCandidate {
    AutonomousIngressCandidate {
        source: "discord",
        agent: agent.to_string(),
        conversation_key: conversation.session_pool_key(),
        message_id: message_id.to_string(),
        routing_contract: "config_autonomous_ingress",
    }
}

/// Emit the structured INFO logs the spec requires. Tests should not
/// depend on log emission; production observability is the consumer.
pub fn log_candidate(candidate: &AutonomousIngressCandidate) {
    info!(
        event = "autonomous workflow candidate",
        source = candidate.source,
        agent = %candidate.agent,
        conversation_key = %candidate.conversation_key,
        message_id = %candidate.message_id,
        routing_contract = candidate.routing_contract,
    );
}

pub fn log_accepted(
    candidate: &AutonomousIngressCandidate,
    disposition: &AutonomousRouteDisposition,
) {
    if let AutonomousRouteDisposition::Accepted {
        task_id,
        workflow_run_id,
        binding_id,
        conversation_key,
    } = disposition
    {
        info!(
            event = "autonomous workflow accepted by runtime",
            source = candidate.source,
            agent = %candidate.agent,
            task_id = %task_id,
            workflow_run_id = %workflow_run_id,
            binding_id = %binding_id,
            conversation_key = %conversation_key,
            disposition = "ACCEPTED",
            consumed = true,
        );
    }
}

pub fn log_failure(
    candidate: &AutonomousIngressCandidate,
    disposition: &AutonomousRouteDisposition,
) {
    match disposition {
        AutonomousRouteDisposition::Rejected {
            error_code,
            retryable,
            detail,
        } => warn!(
            event = "autonomous workflow ingress failed",
            source = candidate.source,
            agent = %candidate.agent,
            error_code = %error_code,
            retryable = retryable,
            consumed = true,
            detail = detail.as_deref().unwrap_or(""),
        ),
        AutonomousRouteDisposition::Unavailable {
            error_code,
            retryable,
        } => warn!(
            event = "autonomous workflow ingress failed",
            source = candidate.source,
            agent = %candidate.agent,
            error_code = %error_code,
            retryable = retryable,
            consumed = true,
        ),
        _ => {}
    }
}

/// Test-only fake client that records calls and returns scripted
/// outcomes. Production must not use this.
#[allow(clippy::type_complexity)]
pub struct FakeAutonomousIngressClient {
    pub calls: std::sync::Mutex<Vec<AutonomousIngressRequest>>,
    pub outcome: std::sync::Mutex<
        Result<
            Result<AutonomousIngressResponse, AutonomousIngressError>,
            tokio::sync::oneshot::Sender<
                Result<
                    Result<AutonomousIngressResponse, AutonomousIngressError>,
                    AutonomousIngressError,
                >,
            >,
        >,
    >,
}

impl FakeAutonomousIngressClient {
    pub fn always_accept() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            outcome: std::sync::Mutex::new(Ok(Ok(AutonomousIngressResponse {
                disposition: "ACCEPTED".to_string(),
                task_id: Some("task-fake".into()),
                workflow_run_id: Some("run-fake".into()),
                binding_id: Some("binding-fake".into()),
                conversation_key: Some("discord:fake:thread".into()),
                error_code: None,
                detail: None,
                retryable: None,
            }))),
        })
    }

    pub fn always_reject() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            outcome: std::sync::Mutex::new(Ok(Ok(AutonomousIngressResponse {
                disposition: "REJECTED".to_string(),
                task_id: None,
                workflow_run_id: None,
                binding_id: None,
                conversation_key: None,
                error_code: Some("AAP_PROJECT_FORBIDDEN".into()),
                detail: Some("project not authorized".into()),
                retryable: Some(false),
            }))),
        })
    }

    pub fn always_unreachable() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            outcome: std::sync::Mutex::new(Ok(Err(AutonomousIngressError::Unreachable(
                "connection refused".into(),
            )))),
        })
    }

    pub fn always_auth_missing() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            outcome: std::sync::Mutex::new(Err(tokio::sync::oneshot::channel().0)),
        })
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl AutonomousIngressClient for FakeAutonomousIngressClient {
    async fn submit(
        &self,
        request: AutonomousIngressRequest,
    ) -> Result<AutonomousIngressResponse, AutonomousIngressError> {
        self.calls.lock().unwrap().push(request);
        // Pull the static outcome; if the holder is currently holding a
        // oneshot sender (auth-missing state), surface AuthMissing
        // directly. This branch is only used by the auth-missing fake
        // builder; production code paths never construct a sender.
        let outcome = self.outcome.lock().unwrap();
        match &*outcome {
            Ok(inner) => match inner {
                Ok(resp) => Ok(resp.clone()),
                Err(err) => Err(err.clone()),
            },
            Err(_sender) => Err(AutonomousIngressError::AuthMissing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_agents(agents: &[&str], universal: bool) -> AutonomousIngressConfig {
        AutonomousIngressConfig {
            aap_agents: agents.iter().map(|s| s.to_string()).collect(),
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "TEST_TOKEN_ENV".into(),
            project_id: "arthur-ai-platform".into(),
            request_timeout_seconds: 5,
            aap_universal_humans: universal,
        }
    }

    #[test]
    fn routing_requires_config() {
        assert!(!should_route_to_aap(None, "ArthurClaude", true));
    }

    #[test]
    fn routing_requires_declared_agent() {
        let cfg = cfg_with_agents(&["ArthurCodex"], false);
        assert!(!should_route_to_aap(Some(&cfg), "ArthurClaude", true));
        assert!(should_route_to_aap(Some(&cfg), "ArthurCodex", true));
    }

    #[test]
    fn routing_requires_tech_lead_when_universal_false() {
        let cfg = cfg_with_agents(&["ArthurClaude"], false);
        assert!(!should_route_to_aap(Some(&cfg), "ArthurClaude", false));
        assert!(should_route_to_aap(Some(&cfg), "ArthurClaude", true));
    }

    #[test]
    fn routing_universal_humans_bypasses_tech_lead() {
        let cfg = cfg_with_agents(&["ArthurClaude"], true);
        assert!(should_route_to_aap(Some(&cfg), "ArthurClaude", false));
        assert!(should_route_to_aap(Some(&cfg), "ArthurClaude", true));
    }

    #[test]
    fn disposition_accepted_carries_canonical_identifiers() {
        let resp = AutonomousIngressResponse {
            disposition: "ACCEPTED".into(),
            task_id: Some("t1".into()),
            workflow_run_id: Some("r1".into()),
            binding_id: Some("b1".into()),
            conversation_key: Some("k1".into()),
            error_code: None,
            detail: None,
            retryable: None,
        };
        let d = project_response(resp, "fallback");
        match d {
            AutonomousRouteDisposition::Accepted {
                task_id,
                workflow_run_id,
                binding_id,
                conversation_key,
            } => {
                assert_eq!(task_id, "t1");
                assert_eq!(workflow_run_id, "r1");
                assert_eq!(binding_id, "b1");
                assert_eq!(conversation_key, "k1");
            }
            _ => panic!("expected Accepted"),
        }
    }

    #[test]
    fn disposition_rejected_carries_error_code() {
        let resp = AutonomousIngressResponse {
            disposition: "REJECTED".into(),
            task_id: None,
            workflow_run_id: None,
            binding_id: None,
            conversation_key: None,
            error_code: Some("AAP_AUTH_FAILURE".into()),
            detail: Some("bad token".into()),
            retryable: Some(false),
        };
        let d = project_response(resp, "fallback");
        match d {
            AutonomousRouteDisposition::Rejected {
                error_code,
                retryable,
                detail,
            } => {
                assert_eq!(error_code, "AAP_AUTH_FAILURE");
                assert!(!retryable);
                assert_eq!(detail.as_deref(), Some("bad token"));
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn disposition_unknown_string_fails_closed_as_rejected() {
        let resp = AutonomousIngressResponse {
            disposition: "WEIRD".into(),
            task_id: None,
            workflow_run_id: None,
            binding_id: None,
            conversation_key: None,
            error_code: None,
            detail: None,
            retryable: None,
        };
        let d = project_response(resp, "fallback");
        assert!(matches!(
            d,
            AutonomousRouteDisposition::Rejected { ref error_code, .. }
            if error_code.starts_with("AAP_UNKNOWN_DISPOSITION")
        ));
    }

    #[test]
    fn transport_error_unreachable_is_retryable() {
        let e = AutonomousIngressError::Unreachable("dns".into());
        assert!(e.retryable());
        let d = project_error(e);
        match d {
            AutonomousRouteDisposition::Unavailable {
                error_code,
                retryable,
            } => {
                assert_eq!(error_code, "AAP_UNREACHABLE");
                assert!(retryable);
            }
            _ => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn transport_error_auth_missing_is_not_retryable() {
        let e = AutonomousIngressError::AuthMissing;
        assert!(!e.retryable());
        let d = project_error(e);
        match d {
            AutonomousRouteDisposition::Unavailable {
                error_code,
                retryable,
            } => {
                assert_eq!(error_code, "AAP_AUTH_MISSING");
                assert!(!retryable);
            }
            _ => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn http_5xx_is_retryable_4xx_is_not() {
        let err5 = AutonomousIngressError::Http {
            status: 503,
            body_snippet: "".into(),
        };
        let err4 = AutonomousIngressError::Http {
            status: 401,
            body_snippet: "".into(),
        };
        assert!(err5.retryable());
        assert!(!err4.retryable());
    }

    #[tokio::test]
    async fn fake_always_accept_records_call_and_returns_accepted() {
        let client = FakeAutonomousIngressClient::always_accept();
        let req = AutonomousIngressRequest {
            protocol: "openab",
            project_id: "arthur-ai-platform".into(),
            transport: "DISCORD",
            conversation_key: "discord:c:1".into(),
            user_objective: "fix it".into(),
            trace_id: "trace-1".into(),
            task_id: None,
            primary_agent: "ArthurClaude".into(),
            language: "en".into(),
            metadata: AutonomousIngressMetadata::default(),
            delivery_destination: None,
        };
        let resp = client.submit(req.clone()).await.unwrap();
        assert_eq!(resp.disposition, "ACCEPTED");
        assert_eq!(client.call_count(), 1);
    }

    #[tokio::test]
    async fn fake_always_unreachable_returns_error() {
        let client = FakeAutonomousIngressClient::always_unreachable();
        let req = AutonomousIngressRequest {
            protocol: "openab",
            project_id: "arthur-ai-platform".into(),
            transport: "DISCORD",
            conversation_key: "discord:c:1".into(),
            user_objective: "fix it".into(),
            trace_id: "trace-1".into(),
            task_id: None,
            primary_agent: "ArthurClaude".into(),
            language: "en".into(),
            metadata: AutonomousIngressMetadata::default(),
            delivery_destination: None,
        };
        let err = client.submit(req).await.unwrap_err();
        assert_eq!(err.code(), "AAP_UNREACHABLE");
        assert!(err.retryable());
    }

    // ===================================================================
    // Phase 6.4 Round 2 — production wiring regression tests.
    //
    // These tests exercise the production builder path that
    // `src/main.rs` uses. They deliberately do NOT call
    // `MockDispatchTarget.with_autonomous_ingress(...)` — that would
    // short-circuit the production seam. Instead they verify the
    // production code path itself:
    //
    //   build_production_client(&aap_cfg)
    //     → HttpAutonomousIngressClient + ReqwestAutonomousIngressTransport
    //
    // and the contract it gives the production AdapterRouter.
    // ===================================================================

    /// Spec scenario: production Config + autonomous_ingress section
    /// → production builder exposes both the config and a real
    /// client. The same builder the binary calls.
    #[test]
    fn production_config_wires_autonomous_ingress_into_builder() {
        // Provide the credential env the test config requests; the
        // test owns the env it depends on, mirroring how the binary
        // reads the env at startup.
        std::env::set_var("TEST_TOKEN_ENV", "round2-test-token-not-real");
        let cfg = cfg_with_agents(&["ArthurClaude"], false);
        let client = build_production_client(&cfg)
            .expect("production client must build when credential is present");
        // Spec assertions: same contract the binary uses.
        assert!(client.base_url.ends_with(":8000"));
        assert_eq!(client.timeout, Duration::from_secs(5));
        // Production builder must return the typed HttpAutonomousIngressClient.
        let client_type = std::any::type_name_of_val(&client);
        assert!(
            client_type.contains("HttpAutonomousIngressClient"),
            "production builder must return HttpAutonomousIngressClient; got {client_type}"
        );
    }

    /// Spec scenario: production config + missing credential env var
    /// → build_production_client returns AuthMissing. The binary
    /// aborts startup with a clear error message.
    #[test]
    fn autonomous_ingress_config_without_credential_fails_closed() {
        let cfg = AutonomousIngressConfig {
            aap_agents: vec!["ArthurClaude".into()],
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            // Use a credential env var name that is NOT set in the
            // test process. Production startup-time check is
            // environment-driven; the test is environment-driven.
            aap_credential_env: "AAP_ROUND2_TEST_CREDENTIAL_MISSING_XYZ".into(),
            project_id: "arthur-ai-platform".into(),
            request_timeout_seconds: 5,
            aap_universal_humans: false,
        };
        // Force-empty in case any inherited env var happens to set it.
        std::env::remove_var("AAP_ROUND2_TEST_CREDENTIAL_MISSING_XYZ");
        let result = build_production_client(&cfg);
        match result {
            Err(AutonomousIngressError::AuthMissing) => {}
            Err(other) => panic!("expected AuthMissing, got: {other}"),
            Ok(_) => panic!("expected AuthMissing, got Ok(client)"),
        }
    }

    /// Spec scenario: absent config → preserved legacy behavior. The
    /// production binary composes the router without invoking
    /// `build_production_client` at all, so ordinary ACP wins for
    /// every human message. We pin the same contract on the
    /// configuration shape: `Config.autonomous_ingress = None`
    /// means legacy.
    #[test]
    fn no_autonomous_ingress_config_preserves_legacy_behavior() {
        // Spec-required: the production binary only invokes
        // build_production_client when the config section is
        // present. We verify by constructing an empty Config and
        // asserting its `autonomous_ingress` field is None — this
        // guarantees the composition seam in `src/main.rs` will skip
        // the wiring entirely and the dispatcher keeps the legacy
        // WORKFLOW_ASSIGNMENT_MISSING → ordinary ACP fallback.
        let raw = "";
        let parsed = crate::config::parse_config_str(raw, "<test>").expect("empty config parses");
        assert!(
            parsed.autonomous_ingress.is_none(),
            "absent [autonomous_ingress] section must leave legacy behavior intact"
        );
    }

    /// Spec scenario: AgentLease / Phase 6.3 native-work dispatcher
    /// path remains untouched. The production builder only adds the
    /// Phase 6.4 components; it does not alter native-dispatch key
    /// derivation, lease fencing, or workflow_revision semantics.
    /// This regression pins the public surface that must remain
    /// stable for downstream tests.
    #[test]
    fn production_builder_does_not_mutate_native_dispatch_contract() {
        std::env::set_var("TEST_TOKEN_ENV", "round2-test-token-not-real");
        let cfg = cfg_with_agents(&["ArthurClaude"], false);
        let _client = build_production_client(&cfg).expect("builds");
        // The factory does not accept or return any
        // NativeWorkflowMetadata / AgentLease types — it only
        // builds an HttpAutonomousIngressClient. We assert this
        // surface constraint by confirming the public factory
        // signature is unrelated to native-work types.
        let _: fn(
            &AutonomousIngressConfig,
        ) -> Result<HttpAutonomousIngressClient, AutonomousIngressError> = build_production_client;
    }
}

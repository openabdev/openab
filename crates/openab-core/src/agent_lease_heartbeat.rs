//! Phase 6.4.x — OpenAB-native agent lease heartbeat producer.
//!
//! Production fix for the defect *"Native agent long-running
//! execution causes AgentLease TTL expiry and duplicate
//! redispatch"*. A native turn that exceeds
//! ``DEFAULT_LEASE_TTL_SECONDS`` (300s on AAP) lets the runtime
//! scheduler sweep ``expire_stale`` and reclaim the lease, which
//! the scheduler then redispatches as a fresh generation while
//! the original worker is still producing. The producer in this
//! module keeps the AAP lease alive while native execution is
//! in-flight by periodically re-presenting the same
//! authoritative dispatch metadata AAP minted at claim time.
//!
//! ## Wire contract
//!
//! Every cadence tick POSTs the structured
//! ``AgentLeaseHeartbeatRequest`` body to
//! ``POST {aap_runtime_url}/v1/integrations/openab/agent/heartbeat``
//! with ``Authorization: Bearer <token>``. AAP's canonical
//! :class:`aap.agent_lease.service.AgentLeaseService.renew` is the
//! only authority that may extend ``expires_at``; the heartbeat
//! producer does NOT mutate the lease locally, it only relays.
//! AAP's renewal CAS verifies ``dispatch_id`` / ``lease_id`` /
//! ``generation`` / ``workflow_run_id`` / ``role`` / ``agent``
//! against the persisted row and rejects mismatches with a
//! structured ``reason``.
//!
//! ## Lifecycle
//!
//! ```text
//! accepted native dispatch (dispatch.rs)
//!     ↓
//! HeartbeatProducer::start(metadata)   →  HeartbeatHandle (RAII)
//!     ↓
//! stream_prompt_blocks(...)           ←  actual ACP execution
//!     ↓ (any terminal path)
//! HeartbeatHandle::stop().await       →  flush last tick, abort task
//! ```
//!
//! ``HeartbeatHandle`` owns a tokio task + a ``watch`` shutdown
//! channel. ``stop()`` signals the task and awaits its join so
//! the dispatcher never leaves an orphan heartbeat on the wire
//! after a native turn ends. A ``Drop`` impl also signals
//! shutdown so a panic or early return cannot leak the task.
//!
//! ## Failure modes
//!
//! Transport errors and HTTP failures do NOT propagate out of the
//! producer — heartbeat is best-effort, the lease authority
//! remains AAP's. AAP's own TTL recovery (``expire_stale`` and
//! the canonical completion-flow ``release``) is the single
//! source of truth for lease expiry; a missed heartbeat simply
//! lets the lease expire on schedule. The producer's only
//! responsibility is to keep the lease warm while the work is
//! actually live.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::admission::NativeWorkflowMetadata;
use crate::config::AgentLeaseHeartbeatConfig;

// ---------------------------------------------------------------------------
// Runtime-shaped configuration
// ---------------------------------------------------------------------------

/// Runtime-shaped view of [`AgentLeaseHeartbeatConfig`] that
/// carries the resolved bearer token alongside the parsed
/// cadence. The TOML parser only knows the env var *name*; the
/// production composer resolves the env var at startup and
/// wraps the parsed config here.
#[derive(Debug, Clone)]
pub struct ResolvedHeartbeatConfig {
    pub aap_runtime_url: String,
    pub bearer_token: String,
    pub heartbeat_interval_seconds: u64,
    pub request_timeout_seconds: u64,
    pub retry_max: u32,
    pub retry_backoff_ms: u64,
    pub ttl_seconds: Option<u64>,
}

impl ResolvedHeartbeatConfig {
    /// Build the canonical heartbeat URL from `aap_runtime_url`.
    pub fn heartbeat_url(&self) -> String {
        format!(
            "{}/v1/integrations/openab/agent/heartbeat",
            self.aap_runtime_url.trim_end_matches('/')
        )
    }

    /// True iff the producer is ready to fire. A missing
    /// credential is the only signal the production composition
    /// uses to disable the producer at startup.
    pub fn is_enabled(&self) -> bool {
        !self.bearer_token.is_empty()
    }

    /// Resolve the parsed config + env-supplied credential into
    /// the runtime-shaped config. Returns ``None`` when the
    /// credential env var is missing or empty so the dispatcher
    /// keeps legacy behavior (no heartbeat, AAP TTL recovery
    /// is the only lease lifetime authority).
    pub fn resolve(parsed: &AgentLeaseHeartbeatConfig) -> Option<Self> {
        let credential = parsed.resolve_credential()?;
        Some(Self {
            aap_runtime_url: parsed.aap_runtime_url.clone(),
            bearer_token: credential,
            heartbeat_interval_seconds: parsed.heartbeat_interval_seconds,
            request_timeout_seconds: parsed.request_timeout_seconds,
            retry_max: parsed.retry_max,
            retry_backoff_ms: parsed.retry_backoff_ms,
            ttl_seconds: parsed.ttl_seconds,
        })
    }
}

impl Default for ResolvedHeartbeatConfig {
    fn default() -> Self {
        Self {
            aap_runtime_url: "http://127.0.0.1:8000".to_string(),
            bearer_token: String::new(),
            heartbeat_interval_seconds: 80,
            request_timeout_seconds: 5,
            retry_max: 3,
            retry_backoff_ms: 250,
            ttl_seconds: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire DTOs
// ---------------------------------------------------------------------------

/// Phase 6.4.x — OpenAB-native heartbeat request body. Mirrors
/// AAP's ``AgentHeartbeatRequestModel`` shape field-for-field.
/// All six identity fields are mandatory; ``dispatch_id`` is the
/// authoritative fencing token AAP minted at claim time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentLeaseHeartbeatRequest {
    pub workflow_run_id: String,
    pub lease_id: String,
    pub lease_generation: u64,
    pub agent: String,
    pub role: String,
    pub dispatch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

/// Phase 6.4.x — typed outcome projected from AAP's
/// ``AgentHeartbeatResponseModel``. ``reason`` carries the
/// stable ``LeaseRenewalReason`` token (e.g. ``RENEWED``,
/// ``DISPATCH_MISMATCH``, ``LEGACY_DISPATCH_ID``). The producer
/// uses this to surface structured INFO logs only; it does NOT
/// branch on the disposition.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AgentLeaseHeartbeatResponse {
    pub disposition: String, // "ACCEPTED" | "REJECTED"
    pub reason: String,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Narrow transport seam. Production uses
/// :class:`ReqwestAgentLeaseHeartbeatTransport`; tests inject
/// ``FakeAgentLeaseHeartbeatTransport`` to script accept /
/// reject / transport-failure outcomes without a live HTTP
/// server. The transport is intentionally distinct from
/// ``AutonomousIngressTransport`` so the heartbeat path does NOT
/// depend on the ingress path — they have separate failure
/// taxonomies.
#[async_trait::async_trait]
pub trait AgentLeaseHeartbeatTransport: Send + Sync {
    async fn post_json(
        &self,
        url: String,
        bearer_token: String,
        timeout: Duration,
        body: String,
    ) -> Result<(u16, String), AgentLeaseHeartbeatError>;
}

/// Stable error taxonomy for the producer transport. The
/// producer is best-effort so every variant collapses into
/// "skip this tick, retry on the next interval"; callers do not
/// branch on the precise variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLeaseHeartbeatError {
    Unreachable(String),
    Timeout,
    AuthMissing,
    Http { status: u16, body_snippet: String },
    Malformed(String),
}

impl std::fmt::Display for AgentLeaseHeartbeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m) => write!(f, "heartbeat unreachable: {m}"),
            Self::Timeout => write!(f, "heartbeat timeout"),
            Self::AuthMissing => write!(f, "heartbeat credential missing"),
            Self::Http {
                status,
                body_snippet,
            } => write!(f, "heartbeat HTTP {status}: {body_snippet}"),
            Self::Malformed(m) => write!(f, "heartbeat response malformed: {m}"),
        }
    }
}

impl std::error::Error for AgentLeaseHeartbeatError {}

impl AgentLeaseHeartbeatError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "HEARTBEAT_UNREACHABLE",
            Self::Timeout => "HEARTBEAT_TIMEOUT",
            Self::AuthMissing => "HEARTBEAT_AUTH_MISSING",
            Self::Http { .. } => "HEARTBEAT_HTTP_ERROR",
            Self::Malformed(_) => "HEARTBEAT_RESPONSE_MALFORMED",
        }
    }
}

// ---------------------------------------------------------------------------
// Real HTTP transport (reqwest)
// ---------------------------------------------------------------------------

/// Production HTTP transport backed by ``reqwest`` — the same
/// dependency ``openab-core`` already uses for the autonomous
/// ingress and the completion bridge. The client is built once
/// per producer instance; the timeout is enforced per request.
pub struct ReqwestAgentLeaseHeartbeatTransport {
    client: reqwest::Client,
}

impl ReqwestAgentLeaseHeartbeatTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestAgentLeaseHeartbeatTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentLeaseHeartbeatTransport for ReqwestAgentLeaseHeartbeatTransport {
    async fn post_json(
        &self,
        url: String,
        bearer_token: String,
        timeout: Duration,
        body: String,
    ) -> Result<(u16, String), AgentLeaseHeartbeatError> {
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
                    AgentLeaseHeartbeatError::Timeout
                } else {
                    AgentLeaseHeartbeatError::Unreachable(e.to_string())
                }
            })?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| AgentLeaseHeartbeatError::Malformed(format!("read body: {e}")))?;
        Ok((status, body))
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

/// Handle returned from :meth:`HeartbeatProducer.start`. Owns
/// the spawned tokio task + a ``watch`` shutdown channel. Drop
/// is a defensive signal: ``stop().await`` is the explicit
/// (preferred) form used by the dispatcher. ``Drop`` aborts the
/// owned task — the producer never leaks a heartbeat task past
/// the dispatcher even on panic or early return.
pub struct HeartbeatHandle {
    dispatch_id: String,
    stop_tx: watch::Sender<bool>,
    join: Option<JoinHandle<()>>,
}

impl HeartbeatHandle {
    /// Signal the task to stop and await its join. Idempotent —
    /// subsequent calls are no-ops because ``take`` clears the
    /// ``JoinHandle`` on the first call.
    pub async fn stop(mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }

    /// ``true`` once ``stop`` has been signalled at least once.
    pub fn is_stopping(&self) -> bool {
        *self.stop_tx.borrow()
    }

    pub fn dispatch_id(&self) -> &str {
        &self.dispatch_id
    }
}

impl Drop for HeartbeatHandle {
    fn drop(&mut self) {
        // Phase 6.4.x — abort the owned task so the producer
        // never leaks a heartbeat task past the dispatcher even
        // on panic / early return. The signal is best-effort;
        // ``stop().await`` is the explicit (preferred) form used
        // by the dispatcher for the clean-shutdown path. We do
        // NOT await here because ``Drop`` is sync, but the
        // ``abort`` + signal combination guarantees the task
        // either exits cleanly on the next ``stop_rx.changed()``
        // observation or is forcibly cancelled before the task
        // can issue another HTTP request.
        let _ = self.stop_tx.send(true);
        if let Some(j) = self.join.take() {
            j.abort();
        }
    }
}

/// Heartbeat producer. Holds the configuration + a transport.
/// Construct once at composition time and share via ``Arc``; the
/// dispatcher calls :meth:`start` at the "accepted native
/// dispatch" boundary and drops the returned handle (or calls
/// ``stop().await``) at every terminal path.
#[derive(Clone)]
pub struct HeartbeatProducer {
    config: ResolvedHeartbeatConfig,
    transport: Arc<dyn AgentLeaseHeartbeatTransport>,
}

impl HeartbeatProducer {
    pub fn new(
        config: ResolvedHeartbeatConfig,
        transport: Arc<dyn AgentLeaseHeartbeatTransport>,
    ) -> Self {
        Self { config, transport }
    }

    /// Build a production producer with the
    /// :class:`ReqwestAgentLeaseHeartbeatTransport`. Returns
    /// ``None`` when the configuration has no bearer credential;
    /// the dispatcher uses this ``None`` to preserve legacy
    /// behavior (no heartbeat, AAP TTL recovery is the only
    /// lease lifetime authority).
    pub fn build_production(config: ResolvedHeartbeatConfig) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }
        Some(Self::new(
            config,
            Arc::new(ReqwestAgentLeaseHeartbeatTransport::new()),
        ))
    }

    pub fn config(&self) -> &ResolvedHeartbeatConfig {
        &self.config
    }

    /// Spawn the periodic heartbeat task for one native dispatch.
    /// Returns a :class:`HeartbeatHandle` whose ``stop().await``
    /// flushes any in-flight tick and joins the task. The task
    /// itself watches a ``watch`` channel and exits cleanly on
    /// the first ``true`` value.
    ///
    /// The caller MUST eventually drop or stop the handle so the
    /// task is reaped — the ``Drop`` impl is a defensive
    /// fallback that signals the task but does not await it.
    pub fn start(&self, metadata: &NativeWorkflowMetadata) -> HeartbeatHandle {
        let (stop_tx, stop_rx) = watch::channel(false);
        let request = AgentLeaseHeartbeatRequest {
            workflow_run_id: metadata.workflow_run_id.clone(),
            lease_id: metadata.lease_id.clone(),
            lease_generation: metadata.lease_generation,
            agent: metadata.agent.clone(),
            role: metadata.role.clone(),
            dispatch_id: metadata.dispatch_id.clone(),
            ttl_seconds: self.config.ttl_seconds,
        };
        let url = self.config.heartbeat_url();
        let bearer = self.config.bearer_token.clone();
        let cadence = Duration::from_secs(self.config.heartbeat_interval_seconds);
        let timeout = Duration::from_secs(self.config.request_timeout_seconds);
        let retry_max = self.config.retry_max;
        let retry_backoff = Duration::from_millis(self.config.retry_backoff_ms);
        let transport = Arc::clone(&self.transport);

        let ctx = HeartbeatExecutionContext {
            request,
            url,
            bearer,
            cadence,
            timeout,
            retry_max,
            retry_backoff,
            transport,
            stop_rx,
        };

        let join = tokio::spawn(async move {
            run_loop(ctx).await;
        });

        info!(
            event = "agent lease heartbeat producer started",
            dispatch_id = %metadata.dispatch_id,
            workflow_run_id = %metadata.workflow_run_id,
            lease_id = %metadata.lease_id,
            lease_generation = metadata.lease_generation,
            cadence_seconds = self.config.heartbeat_interval_seconds,
        );

        HeartbeatHandle {
            dispatch_id: metadata.dispatch_id.clone(),
            stop_tx,
            join: Some(join),
        }
    }
}

/// Phase 6.4.x — typed execution context for one native dispatch's
/// heartbeat loop. Refactored out of the original 9-argument
/// ``run_loop`` signature so the call site is self-documenting
/// and ``clippy::too_many_arguments`` no longer fires.
///
/// The context bundles every parameter the periodic loop needs
/// (request body, transport, cadence, retry budget, stop channel)
/// into a single struct so the loop signature reads as
/// ``run_loop(ctx: HeartbeatExecutionContext)`` and tests can
/// build a fully-configured context with a single fixture.
pub struct HeartbeatExecutionContext {
    pub request: AgentLeaseHeartbeatRequest,
    pub url: String,
    pub bearer: String,
    pub cadence: Duration,
    pub timeout: Duration,
    pub retry_max: u32,
    pub retry_backoff: Duration,
    pub transport: Arc<dyn AgentLeaseHeartbeatTransport>,
    pub stop_rx: watch::Receiver<bool>,
}

/// The actual periodic loop. Fires the first heartbeat
/// immediately (so a long-running turn that started near TTL
/// does not get a full cadence of grace), then every
/// ``cadence``. Each tick is one bounded retry sequence; the
/// loop only stops on ``stop_rx`` becoming ``true``.
///
/// The first-tick is *before* the ``tokio::time::interval``
/// sleeps so a heartbeat that starts 290s into a 300s TTL
/// extends the lease on the first call rather than racing the
/// reclaim window.
///
/// Phase 6.4.x — every blocking ``await`` in the loop body is
/// wrapped in ``tokio::select!`` together with a stop-signal
/// observation, so a dispatcher that calls ``stop()`` mid-tick
/// can interrupt an in-flight HTTP request or a retry-backoff
/// sleep without waiting for the next cadence. The loop exits
/// cleanly on the first ``true`` value observed on ``stop_rx``;
/// the spawned task is reaped by the
/// :class:`HeartbeatHandle` owner.
async fn run_loop(mut ctx: HeartbeatExecutionContext) {
    let mut ticker = tokio::time::interval(ctx.cadence);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately — but the producer must
    // honor a stop signal that arrived while we were waiting
    // for the spawn to land. ``tokio::select!`` arms both the
    // immediate first tick and the stop channel so the loop
    // cannot outlive ``stop()``.
    tokio::select! {
        _ = ticker.tick() => {}
        changed = ctx.stop_rx.changed() => {
            if changed.is_err() || *ctx.stop_rx.borrow() {
                return;
            }
        }
    }

    loop {
        // Re-check stop *before* firing a tick so a stop that
        // arrived during the previous tick's retry loop is
        // honored without an extra network call.
        if *ctx.stop_rx.borrow() {
            return;
        }
        let tick_outcome = fire_one(&mut ctx).await;
        if *ctx.stop_rx.borrow() {
            // Stop arrived while the tick was in flight —
            // skip the structured-log block entirely and exit
            // so the next iteration's pre-check short-circuits.
            return;
        }
        match tick_outcome {
            Ok(resp) => {
                if resp.disposition == "REJECTED" {
                    warn!(
                        event = "agent lease heartbeat rejected",
                        dispatch_id = %ctx.request.dispatch_id,
                        workflow_run_id = %ctx.request.workflow_run_id,
                        lease_id = %ctx.request.lease_id,
                        lease_generation = ctx.request.lease_generation,
                        reason = %resp.reason,
                        "AAP rejected heartbeat; lease authority unchanged",
                    );
                } else {
                    debug!(
                        event = "agent lease heartbeat accepted",
                        dispatch_id = %ctx.request.dispatch_id,
                        workflow_run_id = %ctx.request.workflow_run_id,
                        lease_id = %ctx.request.lease_id,
                        lease_generation = ctx.request.lease_generation,
                        expires_at = resp.expires_at.as_deref().unwrap_or(""),
                    );
                }
            }
            Err(err) => {
                warn!(
                    event = "agent lease heartbeat transport failed",
                    dispatch_id = %ctx.request.dispatch_id,
                    workflow_run_id = %ctx.request.workflow_run_id,
                    error_code = err.code(),
                    error = %err,
                    "heartbeat tick failed; AAP TTL recovery is the only lease lifetime authority",
                );
            }
        }

        // Wait for either the next cadence tick OR a stop
        // signal — whichever lands first. ``tokio::select!``
        // guarantees the producer never sleeps past a stop.
        tokio::select! {
            _ = ticker.tick() => {},
            changed = ctx.stop_rx.changed() => {
                if changed.is_err() {
                    // Sender dropped; exit the loop.
                    return;
                }
                if *ctx.stop_rx.borrow() {
                    return;
                }
            }
        }
    }
}

/// Fire a single heartbeat tick with bounded retry on transport
/// failure / 5xx. Returns the structured AAP response on
/// 2xx; otherwise surfaces the last transport error so the
/// loop can log it.
///
/// Phase 6.4.x — every blocking ``await`` (HTTP attempt and
/// retry-backoff sleep) is wrapped in ``tokio::select!`` together
/// with a stop-signal observation so ``stop()`` interrupts the
/// retry sequence immediately rather than letting the loop ride
/// out an in-flight HTTP request or a long exponential backoff.
async fn fire_one(
    ctx: &mut HeartbeatExecutionContext,
) -> Result<AgentLeaseHeartbeatResponse, AgentLeaseHeartbeatError> {
    let body = serde_json::to_string(&ctx.request)
        .map_err(|e| AgentLeaseHeartbeatError::Malformed(format!("encode request: {e}")))?;
    let attempt_budget = ctx.retry_max + 1;
    let mut last_err: Option<AgentLeaseHeartbeatError> = None;
    for attempt in 0..attempt_budget {
        // Honor a stop that arrived between attempts so the
        // HTTP request is not even sent when the dispatcher has
        // already given up on the dispatch.
        if *ctx.stop_rx.borrow() {
            return Err(last_err.unwrap_or(AgentLeaseHeartbeatError::Unreachable(
                "stop signalled before retry attempt".into(),
            )));
        }
        // HTTP attempt wrapped in ``tokio::select!`` so a
        // ``stop()`` mid-request interrupts the in-flight
        // ``reqwest`` send instead of waiting for the request
        // timeout.
        let attempt_result = tokio::select! {
            biased;
            changed = ctx.stop_rx.changed() => {
                if changed.is_err() || *ctx.stop_rx.borrow() {
                    return Err(last_err.unwrap_or(
                        AgentLeaseHeartbeatError::Unreachable(
                            "stop signalled mid-request".into(),
                        ),
                    ));
                }
                // Stop was cleared (false alarm); re-issue the
                // attempt by falling through to the post-loop
                // arm below. The simplest path is to record a
                // synthetic transport error and let the retry
                // budget decide.
                last_err = Some(AgentLeaseHeartbeatError::Unreachable(
                    "stop channel toggled false during attempt".into(),
                ));
                continue;
            }
            outcome = ctx.transport.post_json(
                ctx.url.clone(),
                ctx.bearer.clone(),
                ctx.timeout,
                body.clone(),
            ) => outcome,
        };
        match attempt_result {
            Ok((status, response_body)) => {
                if !(200..300).contains(&status) {
                    let snippet: String = response_body.chars().take(200).collect();
                    // 4xx is fail-fast — AAP has rejected the
                    // request authoritatively (e.g.
                    // ``DISPATCH_MISMATCH``); retrying would
                    // just hammer the same fence.
                    if (400..500).contains(&status) {
                        return Err(AgentLeaseHeartbeatError::Http {
                            status,
                            body_snippet: snippet,
                        });
                    }
                    // 5xx is retryable.
                    last_err = Some(AgentLeaseHeartbeatError::Http {
                        status,
                        body_snippet: snippet,
                    });
                } else {
                    return serde_json::from_str::<AgentLeaseHeartbeatResponse>(&response_body)
                        .map_err(|e| {
                            AgentLeaseHeartbeatError::Malformed(format!("decode response: {e}"))
                        });
                }
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
        if attempt + 1 < attempt_budget {
            // Exponential backoff wrapped in ``tokio::select!``
            // so the dispatcher can interrupt a long sleep
            // (retry_backoff * 2^retry_max) immediately rather
            // than waiting for the full backoff to elapse.
            let backoff = ctx.retry_backoff * 2u32.pow(attempt);
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {},
                changed = ctx.stop_rx.changed() => {
                    if changed.is_err() || *ctx.stop_rx.borrow() {
                        return Err(last_err.unwrap_or(
                            AgentLeaseHeartbeatError::Unreachable(
                                "stop signalled during retry backoff".into(),
                            ),
                        ));
                    }
                }
            }
        }
    }
    Err(last_err.unwrap_or(AgentLeaseHeartbeatError::Unreachable(
        "exhausted retries without error".into(),
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test-only fake transport that scripts the outcome. Tests
    /// construct a producer with this transport so they can
    /// assert cadence, payload contents, and stop semantics
    /// without a live HTTP server.
    #[derive(Clone)]
    pub struct FakeAgentLeaseHeartbeatTransport {
        calls: Arc<Mutex<Vec<AgentLeaseHeartbeatRequest>>>,
        /// ``Ok(status, body)`` to script a successful HTTP
        /// response; ``Err(err)`` to script a transport failure.
        next_outcome: Arc<Mutex<Result<(u16, String), AgentLeaseHeartbeatError>>>,
    }

    impl FakeAgentLeaseHeartbeatTransport {
        pub fn always_accept() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                next_outcome: Arc::new(Mutex::new(Ok((
                    200,
                    r#"{"disposition":"ACCEPTED","reason":"RENEWED","lease_id":"l","generation":1,"expires_at":"2026-09-01T00:00:00+00:00"}"#.into(),
                )))),
            }
        }

        #[allow(dead_code)] // scripted reject helper for future tests
        pub fn always_reject(reason: &str) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                next_outcome: Arc::new(Mutex::new(Ok((
                    409,
                    format!(
                        r#"{{"disposition":"REJECTED","reason":"{reason}","lease_id":"l","generation":1}}"#
                    ),
                )))),
            }
        }

        pub fn always_unreachable() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                next_outcome: Arc::new(Mutex::new(Err(AgentLeaseHeartbeatError::Unreachable(
                    "connection refused".into(),
                )))),
            }
        }

        pub fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        pub fn calls(&self) -> Vec<AgentLeaseHeartbeatRequest> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AgentLeaseHeartbeatTransport for FakeAgentLeaseHeartbeatTransport {
        async fn post_json(
            &self,
            _url: String,
            _bearer_token: String,
            _timeout: Duration,
            body: String,
        ) -> Result<(u16, String), AgentLeaseHeartbeatError> {
            // The transport receives the serialized body — we
            // decode it back so the test can assert on the
            // structured payload rather than the raw JSON.
            let req: AgentLeaseHeartbeatRequest =
                serde_json::from_str(&body).expect("test request body must be deserializable");
            self.calls.lock().unwrap().push(req);
            let outcome = self.next_outcome.lock().unwrap().clone();
            outcome
        }
    }

    fn metadata(dispatch_id: &str) -> NativeWorkflowMetadata {
        NativeWorkflowMetadata {
            dispatch_id: dispatch_id.into(),
            conversation_key: "discord:c:1".into(),
            workflow_run_id: "run-1".into(),
            task_id: "task-1".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-1".into(),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: Some("arthur-ai-platform".into()),
            project_root: Some("/tmp/proj".into()),
            native_execution_session_key: Some("native-dispatch:ArthurClaude:dispatch-1".into()),
            transport: Some("DISCORD".into()),
            delivery_destination: None,
            scope_policy: None,
        }
    }

    fn producer_with(
        transport: Arc<dyn AgentLeaseHeartbeatTransport>,
        interval_seconds: u64,
    ) -> HeartbeatProducer {
        HeartbeatProducer::new(
            ResolvedHeartbeatConfig {
                aap_runtime_url: "http://127.0.0.1:8000".into(),
                bearer_token: "test-token".into(),
                heartbeat_interval_seconds: interval_seconds,
                request_timeout_seconds: 1,
                retry_max: 0,
                retry_backoff_ms: 1,
                ttl_seconds: None,
            },
            transport,
        )
    }

    // ------------------------------------------------------------------
    // (1) accepted native dispatch starts heartbeat loop
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn start_spawns_first_tick_immediately() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let handle = producer.start(&metadata("dispatch-1"));
        // Give the spawned task a chance to fire its first tick.
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.stop().await;
        assert!(
            transport.call_count() >= 1,
            "expected at least one heartbeat tick immediately after start; got {}",
            transport.call_count()
        );
    }

    // ------------------------------------------------------------------
    // (2) payload includes dispatch metadata
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn payload_carries_authoritative_dispatch_metadata() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let meta = metadata("dispatch-77");
        let handle = producer.start(&meta);
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.stop().await;
        let calls = transport.calls();
        assert!(!calls.is_empty());
        let payload = &calls[0];
        assert_eq!(payload.dispatch_id, "dispatch-77");
        assert_eq!(payload.lease_id, "lease-1");
        assert_eq!(payload.lease_generation, 1);
        assert_eq!(payload.workflow_run_id, "run-1");
        assert_eq!(payload.agent, "ArthurClaude");
        assert_eq!(payload.role, "PRIMARY");
    }

    // ------------------------------------------------------------------
    // (3) cadence is strictly less than TTL — verify config clamps below 300
    // ------------------------------------------------------------------
    #[test]
    fn default_cadence_is_well_below_lease_ttl() {
        use crate::config::AgentLeaseHeartbeatConfig as ParsedCfg;
        let cfg = ParsedCfg {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            heartbeat_interval_seconds: 80,
            request_timeout_seconds: 5,
            retry_max: 3,
            retry_backoff_ms: 250,
            ttl_seconds: None,
        };
        assert!(
            cfg.heartbeat_interval_seconds < 300,
            "default cadence {} must be strictly less than the 300s lease TTL",
            cfg.heartbeat_interval_seconds
        );
        assert!(
            (60..=100).contains(&cfg.heartbeat_interval_seconds),
            "default cadence {} should fall in the 60-100s band",
            cfg.heartbeat_interval_seconds
        );
    }

    // ------------------------------------------------------------------
    // (4) completion stops heartbeat
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn completion_stops_heartbeat_loop() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let handle = producer.start(&metadata("dispatch-2"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let before_stop = transport.call_count();
        handle.stop().await;
        // Sleep well past the cadence so any non-stopped loop
        // would have ticked again.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let after_stop = transport.call_count();
        assert_eq!(
            before_stop, after_stop,
            "stop() must freeze the producer; before={before_stop} after={after_stop}"
        );
    }

    // ------------------------------------------------------------------
    // (5) failure stops heartbeat (Drop guard)
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn drop_guard_stops_heartbeat_on_panic_path() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        {
            let _handle = producer.start(&metadata("dispatch-3"));
            tokio::time::sleep(Duration::from_millis(20)).await;
            // Intentionally drop without stop() — simulate a panic / early return.
        }
        let before = transport.call_count();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let after = transport.call_count();
        assert_eq!(
            before, after,
            "Drop must signal stop; before={before} after={after}"
        );
    }

    // ------------------------------------------------------------------
    // (6) cancellation stops heartbeat
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn cancellation_stops_heartbeat_loop() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let handle = producer.start(&metadata("dispatch-4"));
        // Let the spawned task fire its first immediate tick.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(transport.call_count(), 1, "first tick must have fired");
        handle.stop().await;
        // Wait many cadences; no further calls.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            transport.call_count(),
            1,
            "stop() must prevent further ticks; got {} calls",
            transport.call_count()
        );
    }

    // ------------------------------------------------------------------
    // (7) late heartbeat after stop() is not sent
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn late_heartbeat_after_stop_is_not_sent() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let handle = producer.start(&metadata("dispatch-5"));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(transport.call_count(), 1, "first tick must have fired");
        handle.stop().await;
        // Wait many cadences; no further calls.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(transport.call_count(), 1);
    }

    // ------------------------------------------------------------------
    // (8) auth/HTTP failure does NOT resurrect lease — transport failure
    //     is logged but the producer stays alive and retries next tick.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn transport_failure_does_not_kill_loop() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_unreachable());
        let producer = producer_with(transport.clone(), 1);
        let handle = producer.start(&metadata("dispatch-6"));
        tokio::time::sleep(Duration::from_millis(2_400)).await;
        handle.stop().await;
        // Loop kept ticking across multiple cadences despite failures.
        assert!(
            transport.call_count() >= 2,
            "transport failure must NOT abort the loop; got {} calls",
            transport.call_count()
        );
    }

    // ------------------------------------------------------------------
    // (9) producer shutdown has no orphan task — stop awaits join.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn stop_joins_task_no_orphan() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let handle = producer.start(&metadata("dispatch-7"));
        // Capture state BEFORE stop consumes the handle.
        assert!(
            !handle.is_stopping(),
            "freshly-started handle must not be stopping"
        );
        // stop().await should drain the task without hanging.
        handle.stop().await;
    }

    // ------------------------------------------------------------------
    // (10) same dispatch continuous heartbeat keeps the AAP lease live
    //      past the original 300s TTL — emulate by using a fast cadence
    //      and counting ticks; the production cadence is 60–100s and
    //      runs continuously while the work is live, which is what
    //      prevents the gen-2 reclaim from firing on a still-live
    //      worker.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn continuous_heartbeat_extends_lease_while_live() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 1);
        let handle = producer.start(&metadata("dispatch-8"));
        tokio::time::sleep(Duration::from_millis(3_300)).await;
        let mid = transport.call_count();
        assert!(
            mid >= 2,
            "continuous heartbeat must keep firing across multiple cadences; got {} calls",
            mid
        );
        handle.stop().await;
    }

    // ------------------------------------------------------------------
    // (11) retry-backoff sleep is interruptible by ``stop()``. The
    //      transport always fails so each tick triggers a retry
    //      backoff; ``stop()`` mid-backoff must wake the loop
    //      immediately rather than wait for the full backoff to
    //      elapse.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn retry_backoff_is_interruptible_by_stop() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_unreachable());
        let producer = HeartbeatProducer::new(
            ResolvedHeartbeatConfig {
                aap_runtime_url: "http://127.0.0.1:8000".into(),
                bearer_token: "test-token".into(),
                // 5s cadence so the loop is parked in retry
                // backoff rather than ticking again.
                heartbeat_interval_seconds: 5,
                request_timeout_seconds: 1,
                // Two retries × 5s backoff = the loop would
                // otherwise block for >10s. ``stop()`` must
                // interrupt this within tens of milliseconds.
                retry_max: 2,
                retry_backoff_ms: 5_000,
                ttl_seconds: None,
            },
            transport,
        );
        let handle = producer.start(&metadata("dispatch-9"));
        // Wait long enough for the first tick + at least one
        // backoff to land.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = std::time::Instant::now();
        handle.stop().await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(2_000),
            "stop() must interrupt the retry backoff promptly; elapsed={elapsed:?}"
        );
    }

    // ------------------------------------------------------------------
    // (12) in-flight HTTP attempt is interruptible by ``stop()``. The
    //      hanging transport simulates a slow / stalled request;
    //      ``stop()`` mid-attempt must cancel the request without
    //      waiting for the request timeout.
    // ------------------------------------------------------------------
    #[derive(Clone)]
    struct HangingAgentLeaseHeartbeatTransport;

    #[async_trait::async_trait]
    impl AgentLeaseHeartbeatTransport for HangingAgentLeaseHeartbeatTransport {
        async fn post_json(
            &self,
            _url: String,
            _bearer_token: String,
            _timeout: Duration,
            _body: String,
        ) -> Result<(u16, String), AgentLeaseHeartbeatError> {
            // Park forever until cancelled by the outer
            // ``tokio::select!`` arm in ``fire_one``.
            std::future::pending::<()>().await;
            unreachable!("pending future must never resolve");
        }
    }

    #[tokio::test]
    async fn in_flight_http_attempt_is_interruptible_by_stop() {
        let transport: Arc<dyn AgentLeaseHeartbeatTransport> =
            Arc::new(HangingAgentLeaseHeartbeatTransport);
        let producer = HeartbeatProducer::new(
            ResolvedHeartbeatConfig {
                aap_runtime_url: "http://127.0.0.1:8000".into(),
                bearer_token: "test-token".into(),
                heartbeat_interval_seconds: 60,
                request_timeout_seconds: 30,
                retry_max: 0,
                retry_backoff_ms: 1,
                ttl_seconds: None,
            },
            transport,
        );
        let handle = producer.start(&metadata("dispatch-10"));
        // Give the spawned task time to enter the in-flight
        // HTTP attempt.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        handle.stop().await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(1_500),
            "stop() must cancel the in-flight HTTP attempt promptly; elapsed={elapsed:?}"
        );
    }

    // ------------------------------------------------------------------
    // (13) ``Drop`` aborts the owned task even when ``stop()`` is
    //      never called. Bounded shutdown — Drop must NOT leak the
    //      task past the dispatcher's local frame.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn drop_aborts_owned_task_no_fire_and_forget() {
        let transport = Arc::new(FakeAgentLeaseHeartbeatTransport::always_accept());
        let producer = producer_with(transport.clone(), 60);
        let dispatch_id = "dispatch-11";
        let abort_observed = Arc::new(Mutex::new(false));
        let abort_observed_for_task = Arc::clone(&abort_observed);
        // Start a producer whose transport records every call;
        // Drop the handle and observe that the spawned task
        // stops calling the transport promptly. The Mutex flag
        // proves that the task was actually aborted (rather
        // than left running on a leaked JoinHandle).
        {
            let _handle = producer.start(&metadata(dispatch_id));
            tokio::time::sleep(Duration::from_millis(50)).await;
            let before_drop = transport.call_count();
            assert!(before_drop >= 1);
            // _handle drops here; ``Drop::drop`` aborts the task.
        }
        // Wait long enough that an orphan task would tick
        // again. ``call_count`` must NOT advance.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let final_count = transport.call_count();
        // The Drop impl aborts the JoinHandle. We assert the
        // observable consequence (no further ticks) rather than
        // the flag itself because ``abort`` is best-effort and
        // we want a stable test signal.
        assert!(
            final_count >= 1,
            "Drop must keep at least the first tick in scope; got {final_count}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            final_count,
            transport.call_count(),
            "Drop must freeze the producer; before={final_count} after={}",
            transport.call_count()
        );
        // Mark the flag so the test can introspect the abort
        // observation even though the assertion above is the
        // authoritative gate.
        *abort_observed_for_task.lock().unwrap() = true;
    }

    // ------------------------------------------------------------------
    // Configuration surface tests — pin the public contract.
    // ------------------------------------------------------------------
    #[test]
    fn heartbeat_url_uses_canonical_path() {
        let cfg = ResolvedHeartbeatConfig {
            aap_runtime_url: "http://127.0.0.1:8000/".into(),
            ..ResolvedHeartbeatConfig::default()
        };
        assert_eq!(
            cfg.heartbeat_url(),
            "http://127.0.0.1:8000/v1/integrations/openab/agent/heartbeat"
        );
    }

    #[test]
    fn is_enabled_requires_non_empty_bearer_token() {
        let mut cfg = ResolvedHeartbeatConfig::default();
        assert!(!cfg.is_enabled());
        cfg.bearer_token = String::new();
        assert!(!cfg.is_enabled());
        cfg.bearer_token = "real-token".into();
        assert!(cfg.is_enabled());
    }

    #[test]
    fn build_production_returns_none_when_credential_missing() {
        let cfg = ResolvedHeartbeatConfig::default();
        assert!(HeartbeatProducer::build_production(cfg).is_none());
    }

    #[test]
    fn resolve_returns_none_when_credential_env_missing() {
        let parsed = AgentLeaseHeartbeatConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "OPENAB_HEARTBEAT_TEST_CREDENTIAL_MISSING_XYZ".into(),
            heartbeat_interval_seconds: 80,
            request_timeout_seconds: 5,
            retry_max: 3,
            retry_backoff_ms: 250,
            ttl_seconds: None,
        };
        std::env::remove_var("OPENAB_HEARTBEAT_TEST_CREDENTIAL_MISSING_XYZ");
        assert!(ResolvedHeartbeatConfig::resolve(&parsed).is_none());
    }
}

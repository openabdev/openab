//! OpenAB → AAP completion bridge.
//!
//! Workflow transition ``20260818-openab-automatic-three-agent-handoff``.
//!
//! OpenAB accumulates the final assistant reply in the adapter's
//! ``text_buf`` and observes the ACP terminal ``stop_reason``. At the
//! turn-completion boundary the bridge fires exactly one bounded
//! POST to AAP's ``/v1/integrations/openab/completion`` endpoint.
//! AAP parses the raw text for ``<role_completion>`` and runs the
//! existing trusted auto-handoff pipeline.
//!
//! ## Trust model
//!
//! OpenAB transports an UNTRUSTED completion event. AAP is the sole
//! workflow authority. OpenAB MUST NOT:
//!
//! * decide the workflow transition
//! * infer trusted PASS / FAIL
//! * mutate workflow assignment
//! * provide ``workflow_revision``
//! * derive ``transition_id``
//! * choose the next agent
//! * send the next workflow handoff directly
//!
//! ## Lifecycle
//!
//! The bridge runs at the turn-completion boundary AFTER the normal
//! Discord response delivery. It uses a bounded HTTP timeout and a
//! bounded retry policy. It is NOT a detached ``tokio::spawn`` —
//! the callback is awaited (with a timeout) before the adapter
//! completes the turn. A later persistent outbox can improve crash
//! durability, but Phase 1 is exactly-once within the turn boundary.

use std::time::Duration;

use serde::Serialize;
use tracing::{debug, error, info, warn};

use crate::acp::TurnResult;

/// Configuration for the completion bridge. The bridge is opt-in;
/// when ``enabled = false`` the adapter does not fire any callback.
#[derive(Debug, Clone)]
pub struct CompletionBridgeConfig {
    pub enabled: bool,
    pub url: String,
    pub timeout_ms: u64,
    pub retry_max: u32,
    pub retry_backoff_ms: u64,
    /// Bearer token for the AAP runtime authentication. Empty
    /// means "no auth header" — only acceptable when AAP is
    /// running with authentication disabled (development
    /// only). Production deployments MUST set a real token.
    pub bearer_token: String,
    /// Agent identity carried in the payload. The adapter may
    /// override this from its own configuration when known.
    pub agent_identity: String,
    /// Project identifier (the AAP project's project_id). The
    /// adapter may override this from its own project-aware
    /// routing state when available.
    pub project_id: String,
    /// Project root (canonical absolute path of the project being
    /// worked on). The adapter MUST resolve this from its own
    /// project-aware routing state and MUST NOT default to the
    /// daemon's process cwd.
    pub project_root: String,
}

impl Default for CompletionBridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            timeout_ms: 5_000,
            retry_max: 3,
            retry_backoff_ms: 250,
            bearer_token: String::new(),
            agent_identity: String::new(),
            project_id: String::new(),
            project_root: String::new(),
        }
    }
}

/// The completion event payload. Mirrors AAP's
/// ``OpenABCompletionRequestModel``.
#[derive(Debug, Serialize)]
pub struct CompletionEvent<'a> {
    pub source: &'a str,
    pub agent_identity: &'a str,
    pub session_id: &'a str,
    pub project_id: &'a str,
    pub project_root: &'a str,
    pub raw_assistant_text: &'a str,
    pub channel_id: Option<&'a str>,
    pub thread_id: Option<&'a str>,
    pub openab_turn_id: &'a str,
    pub timestamp: &'a str,
}

/// Outcome of one bridge fire. The adapter logs this; the
/// auto-handoff service on the AAP side is the source of
/// truth for the transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    /// 2xx from AAP.
    Success,
    /// 4xx from AAP (semantic rejection). Do not retry.
    Rejected,
    /// 5xx or transport error after bounded retry exhausted.
    Failed,
}

/// The bridge.
///
/// The bridge is constructed once per OpenAB daemon and is
/// thread-safe (the underlying ``reqwest::Client`` is thread-safe).
#[derive(Clone)]
pub struct CompletionBridge {
    config: CompletionBridgeConfig,
    client: reqwest::Client,
}

impl CompletionBridge {
    /// Construct a bridge from the given configuration. The
    /// ``reqwest::Client`` is constructed with the configured
    /// timeout.
    pub fn new(config: CompletionBridgeConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms.max(1)))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// Whether the bridge is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Fire the callback exactly once for the given turn. The
    /// return value is the outcome (success / rejected / failed).
    ///
    /// The ``openab_turn_id`` MUST be stable across retries so
    /// the AAP transition ledger collapses duplicates.
    pub async fn fire(&self, event: &CompletionEvent<'_>) -> CompletionOutcome {
        if !self.config.enabled {
            debug!("completion bridge disabled; skipping callback");
            return CompletionOutcome::Success;
        }
        if self.config.url.is_empty() {
            warn!(
                "completion bridge enabled but URL is empty; \
                 skipping callback"
            );
            return CompletionOutcome::Success;
        }

        let attempt_budget = self.config.retry_max + 1;
        for attempt in 0..attempt_budget {
            let req = self.client.post(&self.config.url).json(event);
            let req = if self.config.bearer_token.is_empty() {
                req
            } else {
                req.bearer_auth(&self.config.bearer_token)
            };
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(
                        attempt,
                        status = resp.status().as_u16(),
                        "completion callback succeeded"
                    );
                    return CompletionOutcome::Success;
                }
                Ok(resp) if resp.status().is_client_error() => {
                    // 4xx semantic rejection: do not retry. AAP
                    // has already evaluated the request; further
                    // attempts would not change the outcome.
                    warn!(
                        attempt,
                        status = resp.status().as_u16(),
                        "completion callback rejected by AAP; \
                         no retry"
                    );
                    return CompletionOutcome::Rejected;
                }
                Ok(resp) => {
                    warn!(
                        attempt,
                        status = resp.status().as_u16(),
                        "completion callback returned 5xx; \
                         will retry if budget remains"
                    );
                }
                Err(e) => {
                    warn!(
                        attempt,
                        error = %e,
                        "completion callback transport error; \
                         will retry if budget remains"
                    );
                }
            }
            if attempt + 1 < attempt_budget {
                // Exponential backoff: 1x, 2x, 4x, ... base.
                let delay = self.config.retry_backoff_ms * (1u64 << attempt);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
        error!(
            "completion callback exhausted retries for turn {}",
            event.openab_turn_id
        );
        CompletionOutcome::Failed
    }
}

/// Whether the given ``TurnResult`` represents a terminal turn
/// completion. The bridge is fired only for terminal stop
/// reasons; intermediate updates are ignored.
pub fn is_terminal_stop_reason(turn_result: &TurnResult) -> bool {
    matches!(turn_result.stop_reason.as_deref(), Some("end_turn"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::TurnResult;

    fn disabled_bridge() -> CompletionBridge {
        CompletionBridge::new(CompletionBridgeConfig {
            enabled: false,
            ..CompletionBridgeConfig::default()
        })
    }

    fn enabled_bridge_no_url() -> CompletionBridge {
        CompletionBridge::new(CompletionBridgeConfig {
            enabled: true,
            url: String::new(),
            ..CompletionBridgeConfig::default()
        })
    }

    #[test]
    fn fire_returns_success_when_disabled() {
        let bridge = disabled_bridge();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(bridge.fire(&sample_event()));
        assert_eq!(outcome, CompletionOutcome::Success);
    }

    #[test]
    fn fire_returns_success_when_url_empty() {
        let bridge = enabled_bridge_no_url();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(bridge.fire(&sample_event()));
        // The bridge treats "no URL" as a no-op rather than a
        // hard failure. The operator must configure a URL to
        // actually fire callbacks.
        assert_eq!(outcome, CompletionOutcome::Success);
    }

    #[test]
    fn is_terminal_end_turn() {
        let r = TurnResult {
            stop_reason: Some("end_turn".into()),
            ..Default::default()
        };
        assert!(is_terminal_stop_reason(&r));
    }

    #[test]
    fn is_terminal_max_tokens() {
        let r = TurnResult {
            stop_reason: Some("max_tokens".into()),
            ..Default::default()
        };
        assert!(!is_terminal_stop_reason(&r));
    }

    #[test]
    fn is_terminal_refusal() {
        let r = TurnResult {
            stop_reason: Some("refusal".into()),
            ..Default::default()
        };
        assert!(!is_terminal_stop_reason(&r));
    }

    #[test]
    fn is_terminal_error() {
        let r = TurnResult {
            stop_reason: Some("error".into()),
            ..Default::default()
        };
        assert!(!is_terminal_stop_reason(&r));
    }

    #[test]
    fn is_not_terminal_intermediate() {
        let r = TurnResult::default();
        assert!(!is_terminal_stop_reason(&r));
    }

    fn sample_event<'a>() -> CompletionEvent<'a> {
        CompletionEvent {
            source: "openab",
            agent_identity: "ArthurClaude",
            session_id: "sess-1",
            project_id: "arthur-ai-platform",
            project_root: "/home/arthur/workspace/ai-workstation",
            raw_assistant_text: "<role_completion>\n</role_completion>\n",
            channel_id: Some("ch-1"),
            thread_id: Some("th-1"),
            openab_turn_id: "turn-1",
            timestamp: "2026-08-18T13:30:00Z",
        }
    }
}

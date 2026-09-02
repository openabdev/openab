//! Phase 6.4.x — production composition for the OpenAB-native
//! agent lease heartbeat producer.
//!
//! ## Single source of authority — Round 3 correction
//!
//! The heartbeat producer's URL and bearer credential MUST be
//! derived from the canonical `[aap_control_plane]` config.
//! The optional `[agent_lease_heartbeat]` table only supplies
//! cadence / retry / timeout overrides; it MUST NOT duplicate
//! URL or credential authority.
//!
//! Round 2 wired the composer to `[autonomous_ingress]`, but
//! `[autonomous_ingress]` is the **A13 human/direct ingress**
//! authority (Tech-Lead Discord routing) — NOT the AAP
//! scheduler **control-plane** authority. Every native daemon
//! (`openab-claude` / `openab-codex` / `openab-gemini`)
//! unconditionally builds `RuntimeHandler::handle_agent_work`
//! and accepts lease-bound `agent.work` regardless of whether
//! `[autonomous_ingress]` is present. The prior composition
//! rule therefore caused `codex` / `gemini` (no
//! `[autonomous_ingress]`) to accept native work with no
//! heartbeat producer — the exact production defect that
//! motivates this module.
//!
//! Round 3 fixes the authority to `[aap_control_plane]`. All
//! three production daemons either carry this section or rely
//! on its defaults (URL `http://127.0.0.1:8000`, credential
//! env `ARTHUR_AGENT_KEY_OPENAB`, `enabled = true`) and
//! therefore get `Enabled`. The
//! `RuntimeHandler::handle_agent_work` admission seam is the
//! defense-in-depth backstop: lease-bound `agent.work` is
//! rejected when no producer is available.
//!
//! ## Fail-closed rule
//!
//! ```text
//! native work enabled = [aap_control_plane] is present
//!                       AND enabled = true
//! AND heartbeat cannot be derived (credential missing,
//!                                cadence invalid, etc.)
//! → FAIL CLOSED at startup
//! ```
//!
//! ## Native work disabled
//!
//! `[aap_control_plane]` absent, OR `enabled = false` →
//! daemon is configured ACP-only; heartbeat is correctly
//! `Disabled` and the dispatcher's defense-in-depth check
//! rejects any lease-bound `agent.work` that might arrive.
//!
//! ## Cadence band
//!
//! The production cadence band is 60–100s by default
//! (`DEFAULT_HEARTBEAT_INTERVAL_SECONDS = 80`). Cadence MUST
//! be strictly less than AAP's `DEFAULT_LEASE_TTL_SECONDS`
//! (300s); values `>= 300` are rejected at the composition
//! seam so a misconfiguration cannot underprovision heartbeat.

use std::sync::Arc;

use crate::agent_lease_heartbeat::{HeartbeatProducer, ResolvedHeartbeatConfig};
use crate::config::{AapControlPlaneConfig, AgentLeaseHeartbeatConfig};

/// AAP's canonical lease TTL — every producer cadence MUST be
/// strictly below this so a single missed tick cannot let the
/// lease expire before the next tick fires.
pub const AAP_LEASE_TTL_SECONDS: u64 = 300;

/// Default heartbeat cadence when `[agent_lease_heartbeat]`
/// is absent. Mid of the 60–100s band so a transient network
/// blip cannot let the lease expire between two heartbeats.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECONDS: u64 = 80;

/// Default per-request HTTP timeout for one heartbeat POST.
pub const DEFAULT_HEARTBEAT_REQUEST_TIMEOUT_SECONDS: u64 = 5;

/// Default maximum retry attempts per tick.
pub const DEFAULT_HEARTBEAT_RETRY_MAX: u32 = 3;

/// Default initial backoff between retry attempts.
pub const DEFAULT_HEARTBEAT_RETRY_BACKOFF_MS: u64 = 250;

/// Outcome of [`compose_production_heartbeat`] — every variant
/// is observable at startup so the operator sees precisely
/// what composition rule fired. ``Debug`` / ``PartialEq`` are
/// intentionally NOT derived because ``HeartbeatProducer`` wraps
/// an ``Arc<dyn AgentLeaseHeartbeatTransport>`` whose trait
/// object has neither impl; tests use ``matches!`` instead.
#[derive(Clone)]
pub enum HeartbeatComposeOutcome {
    /// Native work is enabled for this daemon AND a heartbeat
    /// producer was successfully built. The ``Some`` carries
    /// the producer ready to inject into every
    /// ``Dispatcher::with_heartbeat_producer`` call.
    Enabled(Arc<HeartbeatProducer>),
    /// Native work is NOT enabled for this daemon
    /// (`[aap_control_plane]` absent OR `enabled = false`).
    /// Heartbeat producer is correctly ``None`` — the
    /// dispatcher accepts only ordinary ACP (or, if a
    /// lease-bound `agent.work` somehow arrives, rejects it
    /// at the admission defense-in-depth seam).
    Disabled,
}

/// Failure modes for the production composition rule. Every
/// variant corresponds to a fail-closed decision: startup MUST
/// abort when ``Err(_)` is returned while native work is
/// enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatComposeError {
    /// The configured `[aap_control_plane]` declares native
    /// work for this daemon but the AAP credential env var is
    /// missing or empty. Refusing to start a dispatcher that
    /// could accept `agent.work` without a heartbeat relay.
    CredentialMissing { credential_env: String },
    /// The optional `[agent_lease_heartbeat]` override set
    /// `heartbeat_interval_seconds` to a value `>=` AAP's
    /// `DEFAULT_LEASE_TTL_SECONDS`. A cadence that does not
    /// strictly beat the TTL is a misconfiguration that would
    /// reproduce the duplicate-redispatch defect.
    CadenceTooLong {
        cadence_seconds: u64,
        ttl_seconds: u64,
    },
    /// `build_production` returned `None` for a non-credential
    /// reason (e.g. internal builder precondition). This is a
    /// programming defect, not a deployable configuration.
    ProducerBuildFailed,
}

/// Whether the daemon is configured to accept AAP-native work.
///
/// True iff `[aap_control_plane]` is present AND `enabled` is
/// `true`. This is the **canonical native-work capability**
/// signal — NOT the presence of `[autonomous_ingress]`, which
/// is the unrelated A13 *human / direct ingress* authority.
///
/// Every production daemon
/// (`openab-claude` / `openab-codex` / `openab-gemini`)
/// unconditionally builds `RuntimeHandler::handle_agent_work`
/// and accepts lease-bound `agent.work`. With this signal, the
/// composer is authoritative for whether heartbeat must be
/// available at runtime.
pub fn native_work_enabled_for(aap_control_plane: Option<&AapControlPlaneConfig>) -> bool {
    aap_control_plane.map(|c| c.enabled).unwrap_or(false)
}

/// Production composition entry point used by `src/main.rs`.
///
/// * `aap_control_plane` — the parsed `[aap_control_plane]`
///   table; `None` means the daemon is legacy-ACP-only (or the
///   operator deliberately set `enabled = false`).
/// * `heartbeat_override` — the parsed `[agent_lease_heartbeat]`
///   table; `None` means default cadence / retry / timeout.
///
/// Returns `Ok(HeartbeatComposeOutcome::Enabled(producer))` when
/// the producer must be wired into every dispatcher. Returns
/// `Ok(HeartbeatComposeOutcome::Disabled)` when native work is
/// disabled and the dispatcher correctly accepts ordinary ACP
/// without a heartbeat relay. Returns `Err(_)` ONLY when the
/// composition rule fails closed — the caller MUST abort
/// startup rather than spin up a dispatcher that could accept
/// `agent.work` without a heartbeat producer.
pub fn compose_production_heartbeat(
    aap_control_plane: Option<&AapControlPlaneConfig>,
    heartbeat_override: Option<&AgentLeaseHeartbeatConfig>,
) -> Result<HeartbeatComposeOutcome, HeartbeatComposeError> {
    // Native work disabled → heartbeat is correctly None.
    if !native_work_enabled_for(aap_control_plane) {
        return Ok(HeartbeatComposeOutcome::Disabled);
    }
    let aap_cp = aap_control_plane
        .expect("native_work_enabled_for returned true so aap_control_plane must be Some");

    // Credential authority is the canonical [aap_control_plane]
    // env var. NEVER accept a credential name override from the
    // deprecated heartbeat override block.
    let bearer_token =
        aap_cp
            .resolve_credential()
            .ok_or_else(|| HeartbeatComposeError::CredentialMissing {
                credential_env: aap_cp.aap_credential_env.clone(),
            })?;

    // Cadence / retry / timeout / ttl from the optional override.
    // The URL is ALWAYS the canonical `[aap_control_plane]` URL
    // — no parallel authority surface.
    let cadence = heartbeat_override
        .map(|h| h.heartbeat_interval_seconds)
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_SECONDS);
    if cadence >= AAP_LEASE_TTL_SECONDS {
        return Err(HeartbeatComposeError::CadenceTooLong {
            cadence_seconds: cadence,
            ttl_seconds: AAP_LEASE_TTL_SECONDS,
        });
    }
    let request_timeout = heartbeat_override
        .map(|h| h.request_timeout_seconds)
        .unwrap_or(DEFAULT_HEARTBEAT_REQUEST_TIMEOUT_SECONDS);
    let retry_max = heartbeat_override
        .map(|h| h.retry_max)
        .unwrap_or(DEFAULT_HEARTBEAT_RETRY_MAX);
    let retry_backoff = heartbeat_override
        .map(|h| h.retry_backoff_ms)
        .unwrap_or(DEFAULT_HEARTBEAT_RETRY_BACKOFF_MS);
    let ttl_seconds = heartbeat_override.and_then(|h| h.ttl_seconds);

    let resolved = ResolvedHeartbeatConfig {
        aap_runtime_url: aap_cp.aap_runtime_url.clone(),
        bearer_token,
        heartbeat_interval_seconds: cadence,
        request_timeout_seconds: request_timeout,
        retry_max,
        retry_backoff_ms: retry_backoff,
        ttl_seconds,
    };

    let producer = HeartbeatProducer::build_production(resolved)
        .ok_or(HeartbeatComposeError::ProducerBuildFailed)?;
    Ok(HeartbeatComposeOutcome::Enabled(Arc::new(producer)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AapControlPlaneConfig;
    use std::sync::Mutex;

    /// Process-wide serialization for env-var mutating tests.
    /// `cargo test` runs tests in parallel threads that share
    /// the process environment, so without this guard one
    /// test's `remove_var` can race another test's
    /// `set_var` and produce spurious failures.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn aap_control_plane_enabled(env: &str) -> AapControlPlaneConfig {
        AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: env.into(),
            enabled: true,
        }
    }

    fn heartbeat_override(cadence: u64) -> AgentLeaseHeartbeatConfig {
        AgentLeaseHeartbeatConfig {
            aap_runtime_url: "http://should-be-ignored:9999".into(),
            aap_credential_env: "SHOULD_BE_IGNORED".into(),
            heartbeat_interval_seconds: cadence,
            request_timeout_seconds: 5,
            retry_max: 3,
            retry_backoff_ms: 250,
            ttl_seconds: None,
        }
    }

    // -----------------------------------------------------------------
    // (1) [aap_control_plane] enabled + valid credential + no override
    //     → producer derived from canonical config and Enabled.
    // -----------------------------------------------------------------
    #[test]
    fn aap_control_plane_enabled_with_valid_credential_derives_producer() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        // SAFETY: `set_var` is marked unsafe in newer Rust; the
        // `ENV_LOCK` mutex above serializes env access across
        // parallel test threads.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "test-bearer-token-123");
        }
        let aap_cp = aap_control_plane_enabled("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("valid canonical config must compose");
        let res = match outcome {
            HeartbeatComposeOutcome::Enabled(producer) => {
                assert!(
                    producer.config().is_enabled(),
                    "derived producer must be enabled when credential is non-empty"
                );
                assert_eq!(
                    producer.config().aap_runtime_url,
                    "http://127.0.0.1:8000",
                    "URL MUST come from aap_control_plane, not any override"
                );
                assert_eq!(
                    producer.config().bearer_token,
                    "test-bearer-token-123",
                    "credential MUST come from aap_control_plane env var"
                );
                assert_eq!(
                    producer.config().heartbeat_interval_seconds,
                    DEFAULT_HEARTBEAT_INTERVAL_SECONDS,
                    "absent override must use the default cadence"
                );
                true
            }
            HeartbeatComposeOutcome::Disabled => false,
        };
        assert!(
            res,
            "native work enabled + valid credential must yield Enabled"
        );
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        }
    }

    // -----------------------------------------------------------------
    // (2) native work enabled + missing credential → fail closed.
    // -----------------------------------------------------------------
    #[test]
    fn native_work_enabled_missing_credential_fails_closed() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let aap_cp = aap_control_plane_enabled("OPENAB_HEARTBEAT_TEST_CREDENTIAL_MISSING_ZZZ");
        // SAFETY: best-effort cleanup of any leftover var from a prior run.
        unsafe {
            std::env::remove_var("OPENAB_HEARTBEAT_TEST_CREDENTIAL_MISSING_ZZZ");
        }
        let err = match compose_production_heartbeat(Some(&aap_cp), None) {
            Err(e) => e,
            Ok(_) => panic!("missing credential MUST fail closed; got Ok(_)"),
        };
        match err {
            HeartbeatComposeError::CredentialMissing { credential_env } => {
                assert_eq!(
                    credential_env, "OPENAB_HEARTBEAT_TEST_CREDENTIAL_MISSING_ZZZ",
                    "error must name the env var the operator should populate"
                );
            }
            other => panic!("expected CredentialMissing; got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // (3) native work disabled → heartbeat can be None without error.
    // -----------------------------------------------------------------
    #[test]
    fn native_work_disabled_returns_disabled_outcome() {
        // (3a) No [aap_control_plane] section at all.
        let outcome = compose_production_heartbeat(None, None)
            .expect("absent section must compose to Disabled, not error");
        assert!(matches!(outcome, HeartbeatComposeOutcome::Disabled));

        // (3b) [aap_control_plane] present but enabled=false.
        let aap_cp_disabled = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ANY".into(),
            enabled: false,
        };
        let outcome = compose_production_heartbeat(Some(&aap_cp_disabled), None)
            .expect("enabled=false must compose to Disabled, not error");
        assert!(matches!(outcome, HeartbeatComposeOutcome::Disabled));
    }

    // -----------------------------------------------------------------
    // (4) invalid cadence → reject config / fail closed.
    // -----------------------------------------------------------------
    #[test]
    fn cadence_equal_to_or_above_ttl_fails_closed() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        // SAFETY: best-effort env cleanup.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "test-bearer-token-123");
        }
        let aap_cp = aap_control_plane_enabled("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        // (4a) cadence == TTL is rejected.
        let outcome_300 =
            compose_production_heartbeat(Some(&aap_cp), Some(&heartbeat_override(300)));
        let err = match outcome_300 {
            Err(e) => e,
            Ok(_) => panic!("cadence equal to TTL MUST fail closed; got Ok(_)"),
        };
        match err {
            HeartbeatComposeError::CadenceTooLong {
                cadence_seconds,
                ttl_seconds,
            } => {
                assert_eq!(cadence_seconds, 300);
                assert_eq!(ttl_seconds, AAP_LEASE_TTL_SECONDS);
            }
            other => panic!("expected CadenceTooLong; got {other:?}"),
        }
        // (4b) cadence > TTL is rejected.
        let outcome_600 =
            compose_production_heartbeat(Some(&aap_cp), Some(&heartbeat_override(600)));
        let err = match outcome_600 {
            Err(e) => e,
            Ok(_) => panic!("cadence above TTL MUST fail closed; got Ok(_)"),
        };
        assert!(matches!(err, HeartbeatComposeError::CadenceTooLong { .. }));

        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        }
    }

    // -----------------------------------------------------------------
    // (5) effective cadence <300 seconds (positive case: well below).
    // -----------------------------------------------------------------
    #[test]
    fn effective_cadence_is_strictly_below_ttl() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let aap_cp = aap_control_plane_enabled("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");

        // (5a) Default cadence — 80s, mid of the 60–100s band.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "test-bearer-token-123");
        }
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("default cadence must compose");
        if let HeartbeatComposeOutcome::Enabled(producer) = outcome {
            assert!(
                producer.config().heartbeat_interval_seconds < AAP_LEASE_TTL_SECONDS,
                "default cadence {} must be strictly less than TTL {}",
                producer.config().heartbeat_interval_seconds,
                AAP_LEASE_TTL_SECONDS
            );
        } else {
            panic!("default cadence path must yield Enabled")
        }

        // (5b) Override cadence at the upper edge of the band — 99s.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "test-bearer-token-123");
        }
        let outcome = compose_production_heartbeat(Some(&aap_cp), Some(&heartbeat_override(99)))
            .expect("99s override must compose");
        if let HeartbeatComposeOutcome::Enabled(producer) = outcome {
            assert_eq!(producer.config().heartbeat_interval_seconds, 99);
            assert!(
                producer.config().heartbeat_interval_seconds < AAP_LEASE_TTL_SECONDS,
                "99s cadence must be strictly less than the 300s lease TTL"
            );
        } else {
            panic!("99s override must yield Enabled")
        }

        // (5c) Override cadence at the lower edge of the band — 60s.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "test-bearer-token-123");
        }
        let outcome = compose_production_heartbeat(Some(&aap_cp), Some(&heartbeat_override(60)))
            .expect("60s override must compose");
        if let HeartbeatComposeOutcome::Enabled(producer) = outcome {
            assert_eq!(producer.config().heartbeat_interval_seconds, 60);
        } else {
            panic!("60s override must yield Enabled")
        }

        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        }
    }

    // -----------------------------------------------------------------
    // Cross-cutting invariants
    // -----------------------------------------------------------------
    #[test]
    fn heartbeat_override_does_not_supplant_canonical_authority() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        // The override block's URL/credential fields are dead
        // when aap_control_plane is the canonical source; the
        // composition MUST source from aap_control_plane even
        // when the override block is present with different
        // values.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK", "canonical-credential");
        }
        let aap_cp = aap_control_plane_enabled("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        let outcome = compose_production_heartbeat(Some(&aap_cp), Some(&heartbeat_override(70)))
            .expect("override + canonical must compose");
        if let HeartbeatComposeOutcome::Enabled(producer) = outcome {
            assert_eq!(
                producer.config().aap_runtime_url,
                "http://127.0.0.1:8000",
                "URL must come from aap_control_plane even when override \
                 declares a different URL"
            );
            assert_eq!(
                producer.config().bearer_token,
                "canonical-credential",
                "credential must come from aap_control_plane env var even \
                 when override declares a different env var name"
            );
            assert_eq!(
                producer.config().heartbeat_interval_seconds,
                70,
                "cadence override must still be honored when URL/credential \
                 are forced to canonical"
            );
        } else {
            panic!("override + canonical must yield Enabled")
        }
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB_TEST_OK");
        }
    }

    #[test]
    fn native_work_enabled_signal_matches_aap_control_plane_enabled_flag() {
        assert!(!native_work_enabled_for(None));
        assert!(!native_work_enabled_for(Some(&AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ANY".into(),
            enabled: false,
        })));
        assert!(native_work_enabled_for(Some(&AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ANY".into(),
            enabled: true,
        })));
    }

    // -----------------------------------------------------------------
    // Round 3 capability composition tests
    // -----------------------------------------------------------------

    /// Test (a): Claude daemon — production routing
    /// `ArthurClaude → openab-claude.sock` — must have heartbeat
    /// ENABLED. Mirrors the deployed
    /// `/home/arthur/openab/claude/config.toml` shape: explicit
    /// `[aap_control_plane]` section + populated credential env.
    #[test]
    fn round_3_claude_daemon_compose_enabled() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB", "claude-deployment-credential");
        }
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            enabled: true,
        };
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("claude daemon config must compose");
        assert!(
            matches!(outcome, HeartbeatComposeOutcome::Enabled(_)),
            "openab-claude MUST be Enabled"
        );
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB");
        }
    }

    /// Test (b): Codex daemon — `ArthurCodex → openab-codex.sock`
    /// — must have heartbeat ENABLED even when no
    /// `[autonomous_ingress]` is configured. The presence of
    /// `[autonomous_ingress]` is irrelevant to the native-work
    /// capability; `[aap_control_plane]` is the canonical
    /// authority.
    #[test]
    fn round_3_codex_daemon_compose_enabled_without_autonomous_ingress() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB", "codex-deployment-credential");
        }
        // Codex config: [aap_control_plane] present + enabled,
        // but [autonomous_ingress] deliberately absent.
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            enabled: true,
        };
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("codex daemon config must compose");
        assert!(
            matches!(outcome, HeartbeatComposeOutcome::Enabled(_)),
            "openab-codex MUST be Enabled even without [autonomous_ingress]"
        );
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB");
        }
    }

    /// Test (c): Gemini daemon — `ArthurGemini → openab-gemini.sock`
    /// — must have heartbeat ENABLED even when no
    /// `[autonomous_ingress]` is configured.
    #[test]
    fn round_3_gemini_daemon_compose_enabled_without_autonomous_ingress() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB", "gemini-deployment-credential");
        }
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            enabled: true,
        };
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("gemini daemon config must compose");
        assert!(
            matches!(outcome, HeartbeatComposeOutcome::Enabled(_)),
            "openab-gemini MUST be Enabled even without [autonomous_ingress]"
        );
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB");
        }
    }

    /// Test (d): ordinary ACP-only daemon — explicit
    /// `[aap_control_plane] enabled = false` — must compose to
    /// `Disabled` without fail-closed.
    #[test]
    fn round_3_acp_only_daemon_with_disabled_flag_composes_disabled() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        // Even with the credential populated, an ACP-only
        // operator setting `enabled = false` MUST disable
        // heartbeat and let the dispatcher accept ordinary ACP.
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB", "would-be-credential");
        }
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            enabled: false,
        };
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("ACP-only daemon must compose to Disabled, not error");
        assert!(
            matches!(outcome, HeartbeatComposeOutcome::Disabled),
            "ACP-only daemon (enabled=false) MUST compose to Disabled"
        );
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB");
        }
    }

    /// Test (e): native-work capable + missing credential →
    /// fail closed. Even though `[aap_control_plane]` is
    /// present and `enabled = true`, a missing
    /// `ARTHUR_AGENT_KEY_OPENAB` env var MUST abort startup.
    #[test]
    fn round_3_native_work_enabled_missing_canonical_credential_fails_closed() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "OPENAB_NATIVE_WORK_CREDENTIAL_MISSING_QQQ".into(),
            enabled: true,
        };
        unsafe {
            std::env::remove_var("OPENAB_NATIVE_WORK_CREDENTIAL_MISSING_QQQ");
        }
        let err = match compose_production_heartbeat(Some(&aap_cp), None) {
            Err(e) => e,
            Ok(_) => panic!("missing canonical credential MUST fail closed; got Ok(_)"),
        };
        assert!(
            matches!(err, HeartbeatComposeError::CredentialMissing { .. }),
            "expected CredentialMissing; got {err:?}"
        );
    }

    /// Test (f): production cadence invariant — every Enabled
    /// outcome resolves cadence strictly below
    /// `AAP_LEASE_TTL_SECONDS`. This is the band that
    /// prevents the duplicate-redispatch defect.
    #[test]
    fn round_3_production_cadence_below_ttl_for_all_enabled() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ARTHUR_AGENT_KEY_OPENAB", "production-cadence-credential");
        }
        let aap_cp = AapControlPlaneConfig {
            aap_runtime_url: "http://127.0.0.1:8000".into(),
            aap_credential_env: "ARTHUR_AGENT_KEY_OPENAB".into(),
            enabled: true,
        };
        // (f1) Default cadence (80s).
        let outcome = compose_production_heartbeat(Some(&aap_cp), None)
            .expect("default cadence must compose");
        if let HeartbeatComposeOutcome::Enabled(producer) = outcome {
            assert!(
                producer.config().heartbeat_interval_seconds < AAP_LEASE_TTL_SECONDS,
                "production default cadence MUST be < 300s; got {}",
                producer.config().heartbeat_interval_seconds
            );
        } else {
            panic!("default cadence path must yield Enabled");
        }
        unsafe {
            std::env::remove_var("ARTHUR_AGENT_KEY_OPENAB");
        }
    }
}

//! Logical agent identity resolution for the OpenAB-native three-agent
//! coding workflow.
//!
//! # Source of truth
//!
//! Each OpenAB daemon process owns one logical identity. The daemon's
//! own identity is sourced from the `ARTHUR_AGENT_NAME` environment
//! variable (the same variable the systemd drop-ins set per agent at
//! [`~/.config/openab/{claude,codex,gemini}.env`]). The mapping from
//! logical name to canonical workflow role is:
//!
//! | Logical name   | Default role |
//! |----------------|---------------|
//! | `ArthurClaude` | `PRIMARY`     |
//! | `ArthurCodex`  | `VERIFIER`    |
//! | `ArthurGemini` | `FINAL_REVIEWER` |
//!
//! The default mapping is the *default* assignment: the actual role
//! at runtime is derived from the project-local
//! [`WorkflowAssignment`](super::assignment::WorkflowAssignment)'s
//! `primary` / `verifier` / `final_reviewer` slots via
//! [`resolve_role_from_assignment`]. A mismatch fails closed so a
//! daemon whose name is not bound to any role slot in the
//! assignment cannot accidentally act as the active role.
//!
//! # Trust boundary
//!
//! The LLM never authorises its own identity or role. Identity
//! comes from the daemon process env (trusted, operator-supplied);
//! role comes from the project-local assignment (trusted,
//! OpenAB-persisted). The validator, the role gate, and the
//! `<workflow_context>` block all consume these trusted values —
//! none of them ask the LLM.
//!
//! # Unavailable agents
//!
//! `assignment.unavailable_agents` lists logical names the Tech Lead
//! has declared offline. [`resolve_role_from_assignment`] still
//! resolves such a name to its role (so the gate logic can surface
//! a structured "you're meant to be active but you're marked
//! unavailable" signal) but marks
//! [`RoleResolution::unavailable`] = `true`.
//!
//! [`~/.config/openab/{claude,codex,gemini}.env`]: (operator-local)

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::assignment::WorkflowAssignment;
use super::state::WorkflowRole;

/// Environment variable the daemon reads its logical identity from.
pub const ARTHUR_AGENT_NAME_ENV: &str = "ARTHUR_AGENT_NAME";

/// Three logical agent identities supported by the OpenAB-native
/// coding workflow. Adding a fourth name requires a coordinated
/// change across this enum, the assignment schema's
/// `primary` / `verifier` / `final_reviewer` slots, and the systemd
/// drop-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentIdentity {
    #[serde(rename = "ArthurClaude")]
    ArthurClaude,
    #[serde(rename = "ArthurCodex")]
    ArthurCodex,
    #[serde(rename = "ArthurGemini")]
    ArthurGemini,
}

impl AgentIdentity {
    pub const ARTHUR_CLAUDE: &'static str = "ArthurClaude";
    pub const ARTHUR_CODEX: &'static str = "ArthurCodex";
    pub const ARTHUR_GEMINI: &'static str = "ArthurGemini";

    /// Parse from the canonical logical name. Strict: any value that
    /// is not one of the three documented names returns
    /// [`IdentityError::UnknownIdentity`].
    pub fn from_str_strict(s: &str) -> Result<Self, IdentityError> {
        match s {
            Self::ARTHUR_CLAUDE => Ok(Self::ArthurClaude),
            Self::ARTHUR_CODEX => Ok(Self::ArthurCodex),
            Self::ARTHUR_GEMINI => Ok(Self::ArthurGemini),
            other => Err(IdentityError::UnknownIdentity(other.to_string())),
        }
    }

    /// Canonical logical name. Stable across releases.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ArthurClaude => Self::ARTHUR_CLAUDE,
            Self::ArthurCodex => Self::ARTHUR_CODEX,
            Self::ArthurGemini => Self::ARTHUR_GEMINI,
        }
    }

    /// The default role this logical name is wired to play in the
    /// canonical `THREE_AGENT` assignment. Token-pressure swaps and
    /// Tech-Lead reassignments override this via the assignment's
    /// `primary` / `verifier` / `final_reviewer` slots, so the
    /// *canonical* role for a daemon at runtime comes from
    /// [`resolve_role_from_assignment`] — this default is only the
    /// fallback used in tests and identity-only paths.
    pub fn default_role(self) -> WorkflowRole {
        match self {
            Self::ArthurClaude => WorkflowRole::Primary,
            Self::ArthurCodex => WorkflowRole::Verifier,
            Self::ArthurGemini => WorkflowRole::FinalReviewer,
        }
    }
}

/// What can go wrong when resolving an identity or assigning a role.
/// `Display` strings are stable diagnostic tokens suitable for
/// audit logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// `ARTHUR_AGENT_NAME` is unset in the daemon's environment.
    EnvUnset,
    /// `ARTHUR_AGENT_NAME` is set but empty.
    EmptyAgentName,
    /// `ARTHUR_AGENT_NAME` has a value that is not one of the
    /// three documented logical names.
    UnknownIdentity(String),
    /// The logical identity is known but does not appear in any of
    /// `assignment.primary` / `verifier` / `final_reviewer`. Fails
    /// closed so a stray daemon process cannot impersonate a role.
    AssignmentMismatch { identity: String },
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvUnset => f.write_str("ARTHUR_AGENT_NAME is unset"),
            Self::EmptyAgentName => f.write_str("ARTHUR_AGENT_NAME is empty"),
            Self::UnknownIdentity(name) => write!(f, "unknown logical identity {name:?}"),
            Self::AssignmentMismatch { identity } => write!(
                f,
                "logical identity {identity:?} is not bound to any role in the workflow assignment"
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

/// The result of comparing an [`AgentIdentity`] against a
/// [`WorkflowAssignment`]'s role slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleResolution {
    /// The role this daemon fills in the workflow.
    pub role: WorkflowRole,
    /// `true` if `assignment.unavailable_agents` lists this
    /// identity's logical name. The role gate still recognises the
    /// daemon as the active slot holder but the role-context
    /// injection may surface this so the LLM knows to report
    /// degraded-mode operation.
    pub unavailable: bool,
}

/// Read the daemon's [`AgentIdentity`] from the process environment.
///
/// Fails closed for:
/// - unset env var ([`IdentityError::EnvUnset`]);
/// - empty env var ([`IdentityError::EmptyAgentName`]);
/// - unrecognised value ([`IdentityError::UnknownIdentity`]).
///
/// Tests that don't set `ARTHUR_AGENT_NAME` should not call this —
/// they should construct [`AgentIdentity`] values directly.
pub fn current_agent_identity_from_env() -> Result<AgentIdentity, IdentityError> {
    // Cache the answer once per process so multiple calls in the
    // same turn don't pay multiple `std::env::var` calls and so
    // tests that override the env see a stable value within a
    // process lifetime. (For tests that need per-call variability,
    // use `AgentIdentity::from_str_strict` directly.)
    static CACHE: OnceLock<Result<AgentIdentity, IdentityError>> = OnceLock::new();
    if let Some(cached) = CACHE.get() {
        return cached.clone();
    }
    let fresh = read_env();
    let _ = CACHE.set(fresh.clone());
    fresh
}

fn read_env() -> Result<AgentIdentity, IdentityError> {
    let name = std::env::var(ARTHUR_AGENT_NAME_ENV).map_err(|_| IdentityError::EnvUnset)?;
    if name.is_empty() {
        return Err(IdentityError::EmptyAgentName);
    }
    AgentIdentity::from_str_strict(&name)
}

/// Resolve this daemon's [`WorkflowRole`] and unavailable-flag from a
/// trusted [`WorkflowAssignment`].
///
/// Returns [`IdentityError::AssignmentMismatch`] when the identity
/// does not appear in any of the assignment's three role slots.
/// This is a fail-closed path — the role gate uses it to suppress
/// daemon processes that are not bound to the assignment.
///
/// Resolution order:
/// 1. Check `assignment.primary` matches [`AgentIdentity::as_str`].
/// 2. Check `assignment.verifier`.
/// 3. Check `assignment.final_reviewer`.
/// 4. Otherwise → [`IdentityError::AssignmentMismatch`].
///
/// `unavailable` is set when [`AgentIdentity::as_str`] appears in
/// `assignment.unavailable_agents`.
pub fn resolve_role_from_assignment(
    identity: AgentIdentity,
    assignment: &WorkflowAssignment,
) -> Result<RoleResolution, IdentityError> {
    let identity_str = identity.as_str();
    let role = if assignment.primary == identity_str {
        WorkflowRole::Primary
    } else if assignment.verifier == identity_str {
        WorkflowRole::Verifier
    } else if assignment.final_reviewer == identity_str {
        WorkflowRole::FinalReviewer
    } else {
        return Err(IdentityError::AssignmentMismatch {
            identity: identity_str.to_string(),
        });
    };
    let unavailable = assignment
        .unavailable_agents
        .iter()
        .any(|a| a == identity_str);
    Ok(RoleResolution { role, unavailable })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arthur_claude_parses() {
        assert_eq!(
            AgentIdentity::from_str_strict("ArthurClaude").unwrap(),
            AgentIdentity::ArthurClaude
        );
        assert_eq!(AgentIdentity::ArthurClaude.as_str(), "ArthurClaude");
    }

    #[test]
    fn arthur_codex_parses() {
        assert_eq!(
            AgentIdentity::from_str_strict("ArthurCodex").unwrap(),
            AgentIdentity::ArthurCodex
        );
        assert_eq!(AgentIdentity::ArthurCodex.as_str(), "ArthurCodex");
    }

    #[test]
    fn arthur_gemini_parses() {
        assert_eq!(
            AgentIdentity::from_str_strict("ArthurGemini").unwrap(),
            AgentIdentity::ArthurGemini
        );
        assert_eq!(AgentIdentity::ArthurGemini.as_str(), "ArthurGemini");
    }

    #[test]
    fn unknown_identity_fails_closed() {
        let cases = [
            "",
            "ArthurOther",
            "arthurclaude",
            " ARTHURCLAUDE ",
            "claude",
        ];
        for s in cases {
            let err =
                AgentIdentity::from_str_strict(s).expect_err(&format!("should fail for {s:?}"));
            assert!(matches!(err, IdentityError::UnknownIdentity(_)));
        }
    }

    #[test]
    fn assignment_role_match_resolves_correctly() {
        // Default THREE_AGENT assignment.
        let a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            std::path::PathBuf::from("/tmp"),
            "ArthurClaude",
            "ArthurCodex",
            "ArthurGemini",
        );
        assert_eq!(
            resolve_role_from_assignment(AgentIdentity::ArthurClaude, &a)
                .unwrap()
                .role,
            WorkflowRole::Primary
        );
        assert_eq!(
            resolve_role_from_assignment(AgentIdentity::ArthurCodex, &a)
                .unwrap()
                .role,
            WorkflowRole::Verifier
        );
        assert_eq!(
            resolve_role_from_assignment(AgentIdentity::ArthurGemini, &a)
                .unwrap()
                .role,
            WorkflowRole::FinalReviewer
        );
    }

    #[test]
    fn assignment_role_name_mismatch_fails_closed() {
        // A four-agent deployment where the daemon running here is
        // ArthurGemini but the assignment lists someone else as
        // final_reviewer.
        let a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            std::path::PathBuf::from("/tmp"),
            "ArthurClaude",
            "ArthurCodex",
            "ArthurOther",
        );
        let err = resolve_role_from_assignment(AgentIdentity::ArthurGemini, &a)
            .expect_err("unbound identity must fail closed");
        assert!(matches!(err, IdentityError::AssignmentMismatch { .. }));
    }

    #[test]
    fn unavailable_agent_remains_resolvable_but_marked() {
        let mut a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            std::path::PathBuf::from("/tmp"),
            "ArthurClaude",
            "ArthurCodex",
            "ArthurGemini",
        );
        a.unavailable_agents.push("ArthurGemini".to_string());

        let r = resolve_role_from_assignment(AgentIdentity::ArthurGemini, &a).unwrap();
        assert_eq!(r.role, WorkflowRole::FinalReviewer);
        assert!(
            r.unavailable,
            "unavailable agent must remain resolvable but marked unavailable"
        );

        let r2 = resolve_role_from_assignment(AgentIdentity::ArthurClaude, &a).unwrap();
        assert!(
            !r2.unavailable,
            "unrelated agent must not be marked unavailable"
        );
    }

    #[test]
    fn token_pressure_swap_resolves_to_swapped_role() {
        // Token-pressure scenario: verifier is unavailable, PRIMARY
        // has been reassigned to Codex.
        let mut a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            std::path::PathBuf::from("/tmp"),
            "ArthurCodex", // <-- PRIMARY now
            "ArthurGemini",
            "ArthurOther",
        );
        a.unavailable_agents.push("ArthurGemini".to_string());
        // The Codex daemon now sees itself as PRIMARY.
        let r = resolve_role_from_assignment(AgentIdentity::ArthurCodex, &a).unwrap();
        assert_eq!(r.role, WorkflowRole::Primary);
        assert!(!r.unavailable);
    }

    #[test]
    fn default_role_mapping_is_three_agent() {
        assert_eq!(
            AgentIdentity::ArthurClaude.default_role(),
            WorkflowRole::Primary
        );
        assert_eq!(
            AgentIdentity::ArthurCodex.default_role(),
            WorkflowRole::Verifier
        );
        assert_eq!(
            AgentIdentity::ArthurGemini.default_role(),
            WorkflowRole::FinalReviewer
        );
    }
}

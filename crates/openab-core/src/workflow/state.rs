//! Canonical workflow state machine for the three-agent coding workflow.
//!
//! OpenAB is the sole workflow authority for this flow (workflow
//! `20260818-openab-automatic-three-agent-handoff`). The types in this
//! module are the *trusted* vocabulary; an LLM-authored claim is treated
//! as untrusted input and validated against these values before any
//! transition runs (validation lands in Phase 2).
//!
//! # Stages
//!
//! The six canonical stages are:
//!
//! - [`WorkflowStage::PrimaryActive`] — `PRIMARY` is the active claimer.
//! - [`WorkflowStage::VerifierActive`] — `VERIFIER` is the active claimer.
//! - [`WorkflowStage::FinalReviewerActive`] — `FINAL_REVIEWER` is the
//!   active claimer.
//! - [`WorkflowStage::PrimaryCorrectionPending`] — bounded defect loop;
//!   the same `PRIMARY` returns for one more cycle.
//! - [`WorkflowStage::TechLeadWait`] — terminal; the Tech Lead owns the
//!   next move.
//! - [`WorkflowStage::Blocked`] — terminal; the workflow cannot
//!   proceed without Tech Lead direction.
//!
//! Terminal stages reject every transition.
//!
//! # Roles
//!
//! [`WorkflowRole`]: `PRIMARY`, `VERIFIER`, `FINAL_REVIEWER`.
//!
//! # Results
//!
//! [`CompletionResult`]: `COMPLETE`, `PASS`, `FAIL`.
//!
//! # Mode
//!
//! [`WorkflowMode`]: `THREE_AGENT`, `TWO_AGENT_DEGRADED`,
//! `SINGLE_AGENT_EMERGENCY`. Degraded-mode routing itself is NOT
//! implemented in Phase 1 — that lands in a later service/identity
//! phase.
//!
//! # Side-effect freedom
//!
//! All helpers in this module are pure: no filesystem, no Discord, no
//! time, no randomness. They are the deterministic core of the state
//! machine and are safe to call from any phase of the workflow.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One of the six canonical workflow stages.
///
/// Serialized as the SCREAMING_SNAKE_CASE string (e.g. `PRIMARY_ACTIVE`)
/// so the on-disk JSON is human-readable and matches the existing
/// ai-workstation `workflow_assignment.json` field naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowStage {
    #[serde(rename = "PRIMARY_ACTIVE")]
    PrimaryActive,
    #[serde(rename = "VERIFIER_ACTIVE")]
    VerifierActive,
    #[serde(rename = "FINAL_REVIEWER_ACTIVE")]
    FinalReviewerActive,
    #[serde(rename = "PRIMARY_CORRECTION_PENDING")]
    PrimaryCorrectionPending,
    #[serde(rename = "TECH_LEAD_WAIT")]
    TechLeadWait,
    #[serde(rename = "BLOCKED")]
    Blocked,
}

impl WorkflowStage {
    /// True for stages that cannot transition further. `TECH_LEAD_WAIT`
    /// and `BLOCKED` both reject every claim.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::TechLeadWait | Self::Blocked)
    }

    /// Parse from the canonical SCREAMING_SNAKE_CASE form. Returns
    /// `None` for any other input so callers can fail closed without
    /// having to special-case unknown variants.
    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "PRIMARY_ACTIVE" => Some(Self::PrimaryActive),
            "VERIFIER_ACTIVE" => Some(Self::VerifierActive),
            "FINAL_REVIEWER_ACTIVE" => Some(Self::FinalReviewerActive),
            "PRIMARY_CORRECTION_PENDING" => Some(Self::PrimaryCorrectionPending),
            "TECH_LEAD_WAIT" => Some(Self::TechLeadWait),
            "BLOCKED" => Some(Self::Blocked),
            _ => None,
        }
    }
}

impl fmt::Display for WorkflowStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PrimaryActive => "PRIMARY_ACTIVE",
            Self::VerifierActive => "VERIFIER_ACTIVE",
            Self::FinalReviewerActive => "FINAL_REVIEWER_ACTIVE",
            Self::PrimaryCorrectionPending => "PRIMARY_CORRECTION_PENDING",
            Self::TechLeadWait => "TECH_LEAD_WAIT",
            Self::Blocked => "BLOCKED",
        })
    }
}

/// One of the three role slots in the workflow assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowRole {
    #[serde(rename = "PRIMARY")]
    Primary,
    #[serde(rename = "VERIFIER")]
    Verifier,
    #[serde(rename = "FINAL_REVIEWER")]
    FinalReviewer,
}

impl WorkflowRole {
    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "PRIMARY" => Some(Self::Primary),
            "VERIFIER" => Some(Self::Verifier),
            "FINAL_REVIEWER" => Some(Self::FinalReviewer),
            _ => None,
        }
    }
}

impl fmt::Display for WorkflowRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Primary => "PRIMARY",
            Self::Verifier => "VERIFIER",
            Self::FinalReviewer => "FINAL_REVIEWER",
        })
    }
}

/// One of the three legal completion results a role can claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompletionResult {
    #[serde(rename = "COMPLETE")]
    Complete,
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "FAIL")]
    Fail,
}

impl CompletionResult {
    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "COMPLETE" => Some(Self::Complete),
            "PASS" => Some(Self::Pass),
            "FAIL" => Some(Self::Fail),
            _ => None,
        }
    }
}

impl fmt::Display for CompletionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Complete => "COMPLETE",
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
        })
    }
}

/// Workflow operating mode. Phase 1 stores the field; the routing
/// behaviour itself (skipping absent roles) lands in a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum WorkflowMode {
    #[serde(rename = "THREE_AGENT")]
    #[default]
    ThreeAgent,
    #[serde(rename = "TWO_AGENT_DEGRADED")]
    TwoAgentDegraded,
    #[serde(rename = "SINGLE_AGENT_EMERGENCY")]
    SingleAgentEmergency,
}

impl fmt::Display for WorkflowMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ThreeAgent => "THREE_AGENT",
            Self::TwoAgentDegraded => "TWO_AGENT_DEGRADED",
            Self::SingleAgentEmergency => "SINGLE_AGENT_EMERGENCY",
        })
    }
}

/// Return the role that is expected to claim at `stage`.
///
/// Returns `None` for terminal stages (no role can claim there).
pub fn expected_role_for_stage(stage: WorkflowStage) -> Option<WorkflowRole> {
    match stage {
        WorkflowStage::PrimaryActive | WorkflowStage::PrimaryCorrectionPending => {
            Some(WorkflowRole::Primary)
        }
        WorkflowStage::VerifierActive => Some(WorkflowRole::Verifier),
        WorkflowStage::FinalReviewerActive => Some(WorkflowRole::FinalReviewer),
        WorkflowStage::TechLeadWait | WorkflowStage::Blocked => None,
    }
}

/// Canonical legal transition table (no side effects).
///
/// Returns the next stage when `(current_stage, claimed_role, claimed_result)`
/// is one of the six documented legal transitions, otherwise `None`.
///
/// Terminal stages reject every input.
pub fn legal_next_stage(
    current: WorkflowStage,
    role: WorkflowRole,
    result: CompletionResult,
) -> Option<WorkflowStage> {
    if current.is_terminal() {
        return None;
    }
    match (current, role, result) {
        (WorkflowStage::PrimaryActive, WorkflowRole::Primary, CompletionResult::Complete) => {
            Some(WorkflowStage::VerifierActive)
        }
        (WorkflowStage::VerifierActive, WorkflowRole::Verifier, CompletionResult::Pass) => {
            Some(WorkflowStage::FinalReviewerActive)
        }
        (WorkflowStage::VerifierActive, WorkflowRole::Verifier, CompletionResult::Fail) => {
            Some(WorkflowStage::PrimaryCorrectionPending)
        }
        (
            WorkflowStage::FinalReviewerActive,
            WorkflowRole::FinalReviewer,
            CompletionResult::Pass,
        ) => Some(WorkflowStage::TechLeadWait),
        (
            WorkflowStage::FinalReviewerActive,
            WorkflowRole::FinalReviewer,
            CompletionResult::Fail,
        ) => Some(WorkflowStage::PrimaryCorrectionPending),
        (
            WorkflowStage::PrimaryCorrectionPending,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        ) => Some(WorkflowStage::VerifierActive),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Test 1: every legal transition ----

    #[test]
    fn every_legal_transition_is_accepted() {
        let cases = [
            (
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
                WorkflowStage::VerifierActive,
            ),
            (
                WorkflowStage::VerifierActive,
                WorkflowRole::Verifier,
                CompletionResult::Pass,
                WorkflowStage::FinalReviewerActive,
            ),
            (
                WorkflowStage::VerifierActive,
                WorkflowRole::Verifier,
                CompletionResult::Fail,
                WorkflowStage::PrimaryCorrectionPending,
            ),
            (
                WorkflowStage::FinalReviewerActive,
                WorkflowRole::FinalReviewer,
                CompletionResult::Pass,
                WorkflowStage::TechLeadWait,
            ),
            (
                WorkflowStage::FinalReviewerActive,
                WorkflowRole::FinalReviewer,
                CompletionResult::Fail,
                WorkflowStage::PrimaryCorrectionPending,
            ),
            (
                WorkflowStage::PrimaryCorrectionPending,
                WorkflowRole::Primary,
                CompletionResult::Complete,
                WorkflowStage::VerifierActive,
            ),
        ];
        for (stage, role, result, expected) in cases {
            assert_eq!(
                legal_next_stage(stage, role, result),
                Some(expected),
                "{stage} + {role}/{result} must transition to {expected}"
            );
        }
    }

    // ---- Test 2: illegal role/result pair ----

    #[test]
    fn illegal_role_result_pair_is_rejected() {
        // PRIMARY cannot claim PASS or FAIL — only COMPLETE.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Pass,
            ),
            None
        );
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Fail,
            ),
            None
        );
        // VERIFIER cannot claim COMPLETE.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::VerifierActive,
                WorkflowRole::Verifier,
                CompletionResult::Complete,
            ),
            None
        );
        // FINAL_REVIEWER cannot claim COMPLETE.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::FinalReviewerActive,
                WorkflowRole::FinalReviewer,
                CompletionResult::Complete,
            ),
            None
        );
    }

    // ---- Test 3: wrong role for stage ----

    #[test]
    fn wrong_role_for_stage_is_rejected() {
        // A VERIFIER claim during PRIMARY_ACTIVE is wrong-role.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryActive,
                WorkflowRole::Verifier,
                CompletionResult::Pass,
            ),
            None
        );
        // A PRIMARY claim during FINAL_REVIEWER_ACTIVE is wrong-role.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::FinalReviewerActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            ),
            None
        );
        // A FINAL_REVIEWER claim during VERIFIER_ACTIVE is wrong-role.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::VerifierActive,
                WorkflowRole::FinalReviewer,
                CompletionResult::Pass,
            ),
            None
        );
    }

    // ---- Test 4: terminal states reject all transitions ----

    #[test]
    fn terminal_states_reject_all_transitions() {
        let roles = [
            WorkflowRole::Primary,
            WorkflowRole::Verifier,
            WorkflowRole::FinalReviewer,
        ];
        let results = [
            CompletionResult::Complete,
            CompletionResult::Pass,
            CompletionResult::Fail,
        ];
        for stage in [WorkflowStage::TechLeadWait, WorkflowStage::Blocked] {
            assert!(stage.is_terminal(), "{stage} must be terminal");
            for role in roles {
                for result in results {
                    assert_eq!(
                        legal_next_stage(stage, role, result),
                        None,
                        "{stage} must reject {role}/{result}"
                    );
                }
            }
        }
    }

    // ---- Test 5: PRIMARY_CORRECTION_PENDING accepts only PRIMARY COMPLETE ----

    #[test]
    fn primary_correction_pending_accepts_only_primary_complete() {
        // The legal accept path.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryCorrectionPending,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            ),
            Some(WorkflowStage::VerifierActive),
        );
        // Every other combination is rejected.
        for role in [
            WorkflowRole::Primary,
            WorkflowRole::Verifier,
            WorkflowRole::FinalReviewer,
        ] {
            for result in [CompletionResult::Pass, CompletionResult::Fail] {
                assert_eq!(
                    legal_next_stage(WorkflowStage::PrimaryCorrectionPending, role, result),
                    None,
                    "PRIMARY_CORRECTION_PENDING must reject {role}/{result}"
                );
            }
        }
        // PRIMARY/COMPLETE was already checked above; the wrong-role + COMPLETE combination is also rejected.
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryCorrectionPending,
                WorkflowRole::Verifier,
                CompletionResult::Complete,
            ),
            None
        );
        assert_eq!(
            legal_next_stage(
                WorkflowStage::PrimaryCorrectionPending,
                WorkflowRole::FinalReviewer,
                CompletionResult::Complete,
            ),
            None
        );
    }

    // ---- Display + from_canonical roundtrip helpers ----

    #[test]
    fn display_matches_canonical_form() {
        let stages = [
            (WorkflowStage::PrimaryActive, "PRIMARY_ACTIVE"),
            (WorkflowStage::VerifierActive, "VERIFIER_ACTIVE"),
            (WorkflowStage::FinalReviewerActive, "FINAL_REVIEWER_ACTIVE"),
            (
                WorkflowStage::PrimaryCorrectionPending,
                "PRIMARY_CORRECTION_PENDING",
            ),
            (WorkflowStage::TechLeadWait, "TECH_LEAD_WAIT"),
            (WorkflowStage::Blocked, "BLOCKED"),
        ];
        for (stage, expected) in stages {
            assert_eq!(stage.to_string(), expected);
            assert_eq!(WorkflowStage::from_canonical(expected), Some(stage));
        }
        assert_eq!(WorkflowStage::from_canonical("NOT_A_STAGE"), None);
    }

    #[test]
    fn serde_uses_canonical_form() {
        let cases = [
            (WorkflowStage::PrimaryActive, "\"PRIMARY_ACTIVE\""),
            (
                WorkflowStage::PrimaryCorrectionPending,
                "\"PRIMARY_CORRECTION_PENDING\"",
            ),
        ];
        for (stage, expected_json) in cases {
            let json = serde_json::to_string(&stage).expect("serialize");
            assert_eq!(json, expected_json);
            let back: WorkflowStage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, stage);
        }
    }

    #[test]
    fn expected_role_for_stage() {
        assert_eq!(
            super::expected_role_for_stage(WorkflowStage::PrimaryActive),
            Some(WorkflowRole::Primary)
        );
        assert_eq!(
            super::expected_role_for_stage(WorkflowStage::VerifierActive),
            Some(WorkflowRole::Verifier)
        );
        assert_eq!(
            super::expected_role_for_stage(WorkflowStage::FinalReviewerActive),
            Some(WorkflowRole::FinalReviewer)
        );
        assert_eq!(
            super::expected_role_for_stage(WorkflowStage::PrimaryCorrectionPending),
            Some(WorkflowRole::Primary)
        );
        assert_eq!(
            super::expected_role_for_stage(WorkflowStage::TechLeadWait),
            None
        );
        assert_eq!(super::expected_role_for_stage(WorkflowStage::Blocked), None);
    }
}

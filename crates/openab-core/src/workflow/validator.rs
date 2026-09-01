//! Trusted validator for an untrusted [`ParsedClaim`] against the
//! project-local [`WorkflowAssignment`].
//!
//! # Trust model
//!
//! The parser in [`super::completion`] returns an UNTRUSTED
//! [`ParsedClaim`]. Every field on it came from the LLM. The
//! validator is the only path that may classify the turn, and it
//! returns a strongly-typed [`ValidationOutcome`] so the future
//! [`WorkflowService`] can branch on enum variants without parsing
//! text tokens.
//!
//! The validator is **pure**. It does not touch the filesystem,
//! Discord, the ledger, or the assignment on disk. The caller is
//! responsible for deriving the [`ReplayState`] from the ledger and
//! passing it in.
//!
//! # Outcome types
//!
//! [`ValidationOutcome`] is an enum with five variants. **Only**
//! [`ValidationOutcome::Accepted`] represents a fresh transition; the
//! other four are distinct so the caller can take distinct actions:
//!
//! | Variant             | Meaning                                          |
//! |---------------------|--------------------------------------------------|
//! | `Accepted`          | Every check passed; safe to commit.              |
//! | `Rejected(reason)`  | Structural check failed (typed `reason`).        |
//! | `AlreadyDelivered`  | Ledger has this `transition_id` in `DELIVERED`.  |
//! | `InFlight`          | Ledger has this `transition_id` in `RESERVED` or |
//! |                     | `PENDING` — a previous attempt is still in       |
//! |                     | progress.                                        |
//! | `FailedPreviously`  | Ledger has this `transition_id` in `FAILED`. The |
//! |                     | audit row is preserved; no auto-retry.           |
//!
//! # The 10 checks
//!
//! Check 1 (assignment exists) is the caller's responsibility. The
//! remaining 9 checks live here:
//!
//! | # | Check                                        | Outcome / Reason            |
//! |---|----------------------------------------------|-----------------------------|
//! | 2 | `workflow_id` matches                        | `Rejected(WorkflowIdMismatch)` |
//! | 3 | `project_id` matches                         | `Rejected(ProjectIdMismatch)`  |
//! | 4 | `project_root` matches                       | `Rejected(ProjectRootMismatch)`|
//! | 5 | `state` is non-terminal                      | `Rejected(TerminalState)`       |
//! | 6 | expected role derived                        | `Rejected(IllegalTransition)`   |
//! | 7 | claimed role == expected                     | `Rejected(RoleMismatch)`        |
//! | 8 | (role, result) is legal                      | `Rejected(IllegalTransition)`   |
//! | 8b| defect-loop cap respected                    | `Accepted { next_stage: BLOCKED }` (terminal) |
//! | 9 | stage transition is legal                    | `Rejected(IllegalTransition)`   |
//! |10 | `transition_id` replay state                 | `AlreadyDelivered` / `InFlight` / `FailedPreviously` / continues |
//!
//! [`ParsedClaim`]: super::completion::ParsedClaim
//! [`WorkflowAssignment`]: super::assignment::WorkflowAssignment
//! [`transition_id`]: super::transition_id::derive_transition_id
//! [`WorkflowService`]: (Phase 4+ — does not exist yet)
//!
//! # Check 8b semantics — fail-closed terminal BLOCKED
//!
//! Phase 4.2.2 promotion: when the validator detects that the
//! claimed transition would enter `PRIMARY_CORRECTION_PENDING`
//! while `defect_loop_count >= SUPPORTED_DEFECT_LOOP_MAX`, it
//! does **not** reject the claim. Instead it returns
//! `Accepted { next_stage: WorkflowStage::Blocked, ... }` so the
//! `WorkflowService` commits a terminal `BLOCKED` transition:
//!
//! - the assignment atomic-persists to `state = BLOCKED`,
//! - `workflow_revision` increments by exactly one,
//! - `last_transition_id` is recorded,
//! - `defect_loop_count` is **not** further incremented
//!   (the cap is the cap — bounded defect loop = 1 is preserved),
//! - `commit_protocol` routes `BLOCKED` to its no-send terminal
//!   branch (no `messenger.send_targeted_activation`),
//! - the validator stays pure — no filesystem, no Discord.
//!
//! Re-routing to `BLOCKED` is fail-closed: subsequent claims see
//! `state == BLOCKED` and are rejected by check 5 with
//! `RejectReason::TerminalState`. This avoids the previous
//! dead-state where `VERIFIER_ACTIVE + defect_loop_count == 1`
//! left a non-terminal workflow with no legal next transition.

use std::fs;
use std::path::Path;

use super::assignment::WorkflowAssignment;
use super::completion::ParsedClaim;
use super::state::{
    expected_role_for_stage, legal_next_stage, CompletionResult, WorkflowRole, WorkflowStage,
};
use super::transition_id::derive_transition_id;

/// Strongly-typed replay state passed in from the ledger. Mirrors
/// the four lifecycle states of [`super::ledger::TransitionStatus`]
/// plus the absent-row case.
///
/// This is NOT the ledger entry itself — it is the *projection* the
/// validator needs to make its decision. Phase 2 has no
/// auto-retry: the validator never derives a `ReplayState` of its
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayState {
    /// No row in the ledger with this `transition_id`.
    New,
    /// Ledger row exists in `DELIVERED` status.
    AlreadyDelivered,
    /// Ledger row exists in `RESERVED` or `PENDING` status.
    InFlight,
    /// Ledger row exists in `FAILED` status. Audit history preserved;
    /// the validator MUST NOT auto-retry.
    FailedPreviously,
}

impl ReplayState {
    /// Convenience constructor: derive [`ReplayState`] from a
    /// ledger status lookup.
    ///
    /// - `None` (no row) → [`ReplayState::New`]
    /// - `Some(Delivered)` → [`ReplayState::AlreadyDelivered`]
    /// - `Some(Reserved)` or `Some(Pending)` → [`ReplayState::InFlight`]
    /// - `Some(Failed)` → [`ReplayState::FailedPreviously`]
    pub fn from_status(status: Option<super::ledger::TransitionStatus>) -> Self {
        use super::ledger::TransitionStatus;
        match status {
            None => Self::New,
            Some(TransitionStatus::Delivered) => Self::AlreadyDelivered,
            Some(TransitionStatus::Reserved) | Some(TransitionStatus::Pending) => Self::InFlight,
            Some(TransitionStatus::Failed) => Self::FailedPreviously,
        }
    }
}

/// Strongly-typed rejection reason. The future [`WorkflowService`]
/// branches on these without parsing text tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// `claim.workflow_id` does not match `assignment.workflow_id`.
    WorkflowIdMismatch,
    /// `claim.project_id` does not match `assignment.project_id`.
    ProjectIdMismatch,
    /// `claim.project_root` does not match canonical
    /// `assignment.project_root`.
    ProjectRootMismatch,
    /// `assignment.state` is terminal (`TECH_LEAD_WAIT` or
    /// `BLOCKED`).
    TerminalState,
    /// The claimed role does not match the expected role for the
    /// current stage.
    RoleMismatch,
    /// `(stage, role, result)` is not in the legal-next-stage table.
    IllegalTransition,
    /// A transition would enter `PRIMARY_CORRECTION_PENDING` while
    /// `assignment.defect_loop_count >= SUPPORTED_DEFECT_LOOP_MAX`.
    DefectLoopExhausted,
}

impl RejectReason {
    /// Stable string token. Suitable for audit logging only — the
    /// future service MUST branch on the enum, not the string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WorkflowIdMismatch => "WORKFLOW_ID_MISMATCH",
            Self::ProjectIdMismatch => "PROJECT_ID_MISMATCH",
            Self::ProjectRootMismatch => "PROJECT_ROOT_MISMATCH",
            Self::TerminalState => "TERMINAL_STATE",
            Self::RoleMismatch => "ROLE_MISMATCH",
            Self::IllegalTransition => "ILLEGAL_TRANSITION",
            Self::DefectLoopExhausted => "DEFECT_LOOP_EXHAUSTED",
        }
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the validator returns. See module-level docs for the five
/// variants and their meanings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationOutcome {
    /// Every check passed. Safe to commit.
    Accepted {
        current_stage: WorkflowStage,
        claimed_role: WorkflowRole,
        claimed_result: CompletionResult,
        next_stage: WorkflowStage,
        transition_id: String,
        new_workflow_revision: u64,
    },
    /// At least one structural check failed. `reason` is a
    /// strongly-typed [`RejectReason`]; `detail` is a free-form
    /// diagnostic for logs.
    Rejected {
        reason: RejectReason,
        detail: String,
    },
    /// The transition_id was already `DELIVERED`. No new commit.
    AlreadyDelivered { transition_id: String },
    /// The transition_id is in `RESERVED` or `PENDING` — a previous
    /// attempt is still in flight. No new commit.
    InFlight { transition_id: String },
    /// The transition_id was previously marked `FAILED`. The audit
    /// row is preserved; Phase 2 never auto-retries. No new commit.
    FailedPreviously { transition_id: String },
}

impl ValidationOutcome {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    /// True for the three replay-state variants (`AlreadyDelivered`,
    /// `InFlight`, `FailedPreviously`).
    pub fn is_replay_outcome(&self) -> bool {
        matches!(
            self,
            Self::AlreadyDelivered { .. } | Self::InFlight { .. } | Self::FailedPreviously { .. }
        )
    }

    /// Borrow the `transition_id` from any outcome that carries one.
    pub fn transition_id(&self) -> Option<&str> {
        match self {
            Self::Accepted { transition_id, .. } => Some(transition_id),
            Self::AlreadyDelivered { transition_id }
            | Self::InFlight { transition_id }
            | Self::FailedPreviously { transition_id } => Some(transition_id),
            Self::Rejected { .. } => None,
        }
    }
}

/// Run the trusted validator.
///
/// Parameters:
/// - `claim`: the untrusted [`ParsedClaim`] from the parser.
/// - `assignment`: the trusted project-local [`WorkflowAssignment`]
///   (loaded from disk by the caller, which is check #1).
/// - `replay`: the [`ReplayState`] derived from the ledger by the
///   caller (check #10). Phase 2 must not be passed a `bool`.
///
/// Order of checks:
/// 1. structural checks 2-9 in order;
/// 2. compute `transition_id` from trusted state;
/// 3. branch on `replay` — only `ReplayState::New` may produce
///    [`ValidationOutcome::Accepted`].
pub fn validate(
    claim: &ParsedClaim,
    assignment: &WorkflowAssignment,
    replay: ReplayState,
) -> ValidationOutcome {
    // Check 2: workflow_id matches.
    if claim.workflow_id != assignment.workflow_id {
        return reject(
            RejectReason::WorkflowIdMismatch,
            format!(
                "claim={:?} assignment={:?}",
                claim.workflow_id, assignment.workflow_id
            ),
        );
    }

    // Check 3: project_id matches.
    if claim.project_id != assignment.project_id {
        return reject(
            RejectReason::ProjectIdMismatch,
            format!(
                "claim={:?} assignment={:?}",
                claim.project_id, assignment.project_id
            ),
        );
    }

    // Check 4: project_root matches. Compare canonical forms so
    // `/a/./b` vs `/a/b` cannot smuggle a path mismatch past us.
    let claim_canonical =
        fs::canonicalize(&claim.project_root).unwrap_or_else(|_| claim.project_root.clone());
    if claim_canonical != assignment.project_root {
        return reject(
            RejectReason::ProjectRootMismatch,
            format!(
                "claim={:?} (canonical={:?}) assignment={:?}",
                claim.project_root, claim_canonical, assignment.project_root
            ),
        );
    }

    // Check 5: state is non-terminal.
    if assignment.state.is_terminal() {
        return reject(
            RejectReason::TerminalState,
            format!("state={}", assignment.state),
        );
    }

    // Check 6: derive expected role from the current stage. For
    // non-terminal stages this is always Some. If it ever returns
    // None for a non-terminal stage, that's a programmer error
    // (state enum has drifted from the helper), so fail closed.
    let expected_role = match expected_role_for_stage(assignment.state) {
        Some(r) => r,
        None => {
            return reject(
                RejectReason::IllegalTransition,
                format!("no expected role for stage {}", assignment.state),
            );
        }
    };

    // Check 7: claimed role == expected role.
    if claim.role != expected_role {
        return reject(
            RejectReason::RoleMismatch,
            format!(
                "stage={} expected={} claimed={}",
                assignment.state, expected_role, claim.role
            ),
        );
    }

    // Check 8 + 9: legal-next-stage is the canonical authority for
    // (role, result) legality and stage-to-stage legality. A None
    // return covers both.
    let next_stage = match legal_next_stage(assignment.state, claim.role, claim.result) {
        Some(s) => s,
        None => {
            return reject(
                RejectReason::IllegalTransition,
                format!(
                    "no legal transition for stage={} role={} result={}",
                    assignment.state, claim.role, claim.result
                ),
            );
        }
    };

    // Check 8b: defect-loop cap. Phase 4.2.2 fail-closed
    // promotion: when the validator detects the claimed transition
    // would ENTER `PRIMARY_CORRECTION_PENDING` while the cap is
    // already exhausted, it re-routes `next_stage` to the
    // terminal `WorkflowStage::Blocked`. The validator stays
    // pure — no filesystem, no Discord. The Service's
    // `commit_protocol` recognises `Blocked` as the documented
    // terminal-no-send branch (see `service.rs::commit_protocol`
    // Step C / Step F), atomic-persists the assignment with
    // `state = BLOCKED`, increments `workflow_revision` by
    // exactly one, records `last_transition_id`, and refrains
    // from invoking the messenger. The validator never
    // increments `defect_loop_count`; the cap remains
    // `SUPPORTED_DEFECT_LOOP_MAX = 1` (no unbounded drift).
    let effective_next_stage = if next_stage == WorkflowStage::PrimaryCorrectionPending
        && assignment.defect_loop_count >= super::assignment::SUPPORTED_DEFECT_LOOP_MAX
    {
        WorkflowStage::Blocked
    } else {
        next_stage
    };

    // Check 10: transition_id replay state. Derived from trusted
    // state, never supplied by the agent.
    let transition_id = derive_transition_id(
        &assignment.workflow_id,
        assignment.workflow_revision,
        assignment.state,
        claim.role,
        claim.result,
    );

    match replay {
        ReplayState::AlreadyDelivered => ValidationOutcome::AlreadyDelivered { transition_id },
        ReplayState::InFlight => ValidationOutcome::InFlight { transition_id },
        ReplayState::FailedPreviously => ValidationOutcome::FailedPreviously { transition_id },
        ReplayState::New => ValidationOutcome::Accepted {
            current_stage: assignment.state,
            claimed_role: claim.role,
            claimed_result: claim.result,
            next_stage: effective_next_stage,
            transition_id,
            new_workflow_revision: assignment.workflow_revision + 1,
        },
    }
}

fn reject(reason: RejectReason, detail: String) -> ValidationOutcome {
    ValidationOutcome::Rejected { reason, detail }
}

/// Best-effort canonicalization used by check #4. If the path does
/// not exist (e.g. the agent wrote a typo), the original path is
/// compared verbatim. The validator's check #4 still rejects on
/// mismatch — the canonicalize only collapses equivalent spellings.
pub fn canonical_root_or_original(p: &Path) -> std::path::PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::completion::ParsedClaim;
    use crate::workflow::state::{CompletionResult, WorkflowRole, WorkflowStage};
    use chrono::Utc;
    use std::path::PathBuf;

    fn assignment_at(stage: WorkflowStage, project_root: PathBuf) -> WorkflowAssignment {
        WorkflowAssignment {
            schema_version: "v2".into(),
            workflow_id: "wf-001".into(),
            project_id: "openab".into(),
            project_root,
            mode: Default::default(),
            primary: "ArthurClaude".into(),
            verifier: "ArthurCodex".into(),
            final_reviewer: "ArthurGemini".into(),
            state: stage,
            workflow_revision: 0,
            defect_loop_count: 0,
            language: "zh-TW".into(),
            thread_id: "1536735741642547262".into(),
            last_transition_id: None,
            last_delivery_message_id: None,
            unavailable_agents: Vec::new(),
            authorized_by: "Tech Lead".into(),
            reason: "test".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn claim_with(
        role: WorkflowRole,
        result: CompletionResult,
        workflow_id: &str,
        project_id: &str,
        project_root: &str,
    ) -> ParsedClaim {
        ParsedClaim {
            role,
            result,
            workflow_id: workflow_id.into(),
            project_id: project_id.into(),
            project_root: PathBuf::from(project_root),
            scope: None,
            timestamp: None,
        }
    }

    // ---- existing structural checks (now use ReplayState::New) ----

    #[test]
    fn valid_primary_complete_in_primary_active_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                next_stage,
                transition_id,
                new_workflow_revision,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::VerifierActive);
                assert_eq!(new_workflow_revision, 1);
                assert_eq!(transition_id.len(), 32);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn workflow_id_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-WRONG",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, RejectReason::WorkflowIdMismatch);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn project_id_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "wrong-project",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, RejectReason::ProjectIdMismatch);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn project_root_mismatch_is_rejected() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let canonical_a = std::fs::canonicalize(dir_a.path()).unwrap();
        let canonical_b = std::fs::canonicalize(dir_b.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical_a);
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical_b.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, RejectReason::ProjectRootMismatch);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn terminal_state_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        for stage in [WorkflowStage::TechLeadWait, WorkflowStage::Blocked] {
            let a = assignment_at(stage, canonical.clone());
            let c = claim_with(
                WorkflowRole::Primary,
                CompletionResult::Complete,
                "wf-001",
                "openab",
                canonical.to_str().unwrap(),
            );
            match validate(&c, &a, ReplayState::New) {
                ValidationOutcome::Rejected { reason, .. } => {
                    assert_eq!(reason, RejectReason::TerminalState);
                }
                other => panic!("expected Rejected for {stage}, got {other:?}"),
            }
        }
    }

    #[test]
    fn wrong_role_for_stage_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Pass,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, RejectReason::RoleMismatch);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn illegal_result_for_role_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Pass,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(reason, RejectReason::IllegalTransition);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn primary_correction_pending_accepts_primary_complete_with_zero_count() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryCorrectionPending, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(next_stage, WorkflowStage::VerifierActive);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn verifier_fail_transitions_to_primary_correction_pending_with_zero_count() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(next_stage, WorkflowStage::PrimaryCorrectionPending);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn final_reviewer_pass_transitions_to_tech_lead_wait() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::FinalReviewerActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::FinalReviewer,
            CompletionResult::Pass,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                next_stage,
                new_workflow_revision,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::TechLeadWait);
                assert_eq!(new_workflow_revision, 1);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn revision_increment_produces_distinct_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        let id1 = match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { transition_id, .. } => transition_id,
            other => panic!("first call: expected Accepted, got {other:?}"),
        };
        a.state = WorkflowStage::VerifierActive;
        a.workflow_revision = 1;
        let id2 = match validate(
            &claim_with(
                WorkflowRole::Verifier,
                CompletionResult::Pass,
                "wf-001",
                "openab",
                canonical.to_str().unwrap(),
            ),
            &a,
            ReplayState::New,
        ) {
            ValidationOutcome::Accepted { transition_id, .. } => transition_id,
            other => panic!("second call: expected Accepted, got {other:?}"),
        };
        assert_ne!(id1, id2);
    }

    // ---- ReplayState tests (Issue 1) ----

    #[test]
    fn new_replay_state_with_no_ledger_row_produces_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { transition_id, .. } => {
                assert_eq!(transition_id.len(), 32);
            }
            other => panic!("expected Accepted for ReplayState::New, got {other:?}"),
        }
    }

    #[test]
    fn replay_state_already_delivered_produces_already_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::AlreadyDelivered) {
            ValidationOutcome::AlreadyDelivered { transition_id } => {
                assert_eq!(transition_id.len(), 32);
            }
            other => panic!("expected AlreadyDelivered, got {other:?}"),
        }
    }

    #[test]
    fn replay_state_in_flight_produces_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::InFlight) {
            ValidationOutcome::InFlight { transition_id } => {
                assert_eq!(transition_id.len(), 32);
            }
            other => panic!("expected InFlight, got {other:?}"),
        }
    }

    #[test]
    fn replay_state_failed_previously_produces_failed_previously() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::FailedPreviously) {
            ValidationOutcome::FailedPreviously { transition_id } => {
                assert_eq!(transition_id.len(), 32);
            }
            other => panic!("expected FailedPreviously, got {other:?}"),
        }
    }

    #[test]
    fn replay_state_in_flight_distinct_from_already_delivered() {
        // Same transition_id for both — the validator surfaces them
        // as distinct variants so the future service can branch
        // without parsing text.
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::PrimaryActive, canonical.clone());
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        let ad = validate(&c, &a, ReplayState::AlreadyDelivered);
        let inflight = validate(&c, &a, ReplayState::InFlight);
        assert_ne!(ad, inflight);
        assert!(matches!(ad, ValidationOutcome::AlreadyDelivered { .. }));
        assert!(matches!(inflight, ValidationOutcome::InFlight { .. }));
        assert!(ad.transition_id() == inflight.transition_id());
    }

    // ---- Defect-loop tests (Issue 3) ----

    #[test]
    fn defect_loop_zero_with_verifier_fail_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        assert_eq!(a.defect_loop_count, 0);
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(next_stage, WorkflowStage::PrimaryCorrectionPending);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn defect_loop_one_with_verifier_fail_routes_to_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        // Phase 4.2.2: defect-loop exhaustion is re-routed to
        // terminal BLOCKED, not rejected. The validator stays
        // pure; the service is responsible for the BLOCKED
        // commit path (no Discord send, atomic persist).
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                next_stage,
                new_workflow_revision,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::Blocked);
                assert_eq!(
                    new_workflow_revision,
                    a.workflow_revision + 1,
                    "revision must increment exactly once"
                );
            }
            other => panic!("expected Accepted(next_stage=BLOCKED), got {other:?}"),
        }
    }

    #[test]
    fn defect_loop_zero_with_final_reviewer_fail_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::FinalReviewerActive, canonical.clone());
        assert_eq!(a.defect_loop_count, 0);
        let c = claim_with(
            WorkflowRole::FinalReviewer,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(next_stage, WorkflowStage::PrimaryCorrectionPending);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn defect_loop_one_with_final_reviewer_fail_routes_to_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::FinalReviewerActive, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::FinalReviewer,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                next_stage,
                new_workflow_revision,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::Blocked);
                assert_eq!(
                    new_workflow_revision,
                    a.workflow_revision + 1,
                    "revision must increment exactly once on BLOCKED re-route"
                );
            }
            other => panic!("expected Accepted(next_stage=BLOCKED), got {other:?}"),
        }
    }

    #[test]
    fn primary_correction_pending_with_one_count_accepts_primary_complete() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::PrimaryCorrectionPending, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(next_stage, WorkflowStage::VerifierActive);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn second_failure_after_correction_with_one_count_routes_to_blocked() {
        // Simulate: count=1, in PRIMARY_CORRECTION_PENDING; PRIMARY
        // re-runs (count stays 1, validator doesn't mutate); then
        // VERIFIER fails again with count=1 — must re-route to
        // terminal BLOCKED, not reject.
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                next_stage,
                new_workflow_revision,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::Blocked);
                assert_eq!(new_workflow_revision, a.workflow_revision + 1);
            }
            other => panic!("expected Accepted(next_stage=BLOCKED), got {other:?}"),
        }
    }

    #[test]
    fn validator_does_not_mutate_defect_loop_count() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        a.defect_loop_count = 0;
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        let before = a.defect_loop_count;
        let _ = validate(&c, &a, ReplayState::New);
        assert_eq!(
            a.defect_loop_count, before,
            "validator must not mutate defect_loop_count"
        );
    }

    // ---- Phase 4.2.2 DefectLoopExhausted → BLOCKED ----

    #[test]
    fn blocked_re_route_uses_deterministic_transition_id() {
        // The re-route to BLOCKED must NOT change the
        // transition_id: it's still derived from the same
        // trusted (workflow_id, revision, current_stage, role,
        // result) tuple, so Phase 4.2 recovery primitives that
        // inspect the ledger continue to see a stable id.
        use crate::workflow::transition_id::derive_transition_id;
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        let expected_id = derive_transition_id(
            "wf-001",
            a.workflow_revision,
            WorkflowStage::VerifierActive,
            WorkflowRole::Verifier,
            CompletionResult::Fail,
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted {
                transition_id,
                next_stage,
                ..
            } => {
                assert_eq!(next_stage, WorkflowStage::Blocked);
                assert_eq!(
                    transition_id, expected_id,
                    "BLOCKED re-route must keep transition_id stable"
                );
            }
            other => panic!("expected Accepted(next_stage=BLOCKED), got {other:?}"),
        }
    }

    #[test]
    fn blocked_terminal_state_rejects_subsequent_claims() {
        // After the BLOCKED commit lands, the assignment is
        // terminal. Subsequent claims are rejected by check 5
        // (`TerminalState`), not by DefectLoopExhausted. The
        // bounded defect-loop cap is preserved (count not
        // mutated by validator).
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut a = assignment_at(WorkflowStage::Blocked, canonical.clone());
        a.defect_loop_count = 1;
        let c = claim_with(
            WorkflowRole::Primary,
            CompletionResult::Complete,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Rejected { reason, .. } => {
                assert_eq!(
                    reason,
                    RejectReason::TerminalState,
                    "terminal BLOCKED must reject via TerminalState, not DefectLoopExhausted"
                );
            }
            other => panic!("expected Rejected(TerminalState), got {other:?}"),
        }
    }

    #[test]
    fn defect_loop_zero_routes_to_primary_correction_pending_not_blocked() {
        // Negative control: count=0 must NOT short-circuit to
        // BLOCKED. The validator must preserve the normal
        // PrimaryCorrectionPending transition when budget remains.
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let a = assignment_at(WorkflowStage::VerifierActive, canonical.clone());
        assert_eq!(a.defect_loop_count, 0);
        let c = claim_with(
            WorkflowRole::Verifier,
            CompletionResult::Fail,
            "wf-001",
            "openab",
            canonical.to_str().unwrap(),
        );
        match validate(&c, &a, ReplayState::New) {
            ValidationOutcome::Accepted { next_stage, .. } => {
                assert_eq!(
                    next_stage,
                    WorkflowStage::PrimaryCorrectionPending,
                    "count=0 must take the documented PrimaryCorrectionPending path"
                );
            }
            other => panic!("expected Accepted(PrimaryCorrectionPending), got {other:?}"),
        }
    }

    // ---- ReplayState::from_status mapping (Issue 1) ----

    #[test]
    fn replay_state_from_status_maps_correctly() {
        use crate::workflow::ledger::TransitionStatus;
        assert_eq!(ReplayState::from_status(None), ReplayState::New);
        assert_eq!(
            ReplayState::from_status(Some(TransitionStatus::Delivered)),
            ReplayState::AlreadyDelivered
        );
        assert_eq!(
            ReplayState::from_status(Some(TransitionStatus::Reserved)),
            ReplayState::InFlight
        );
        assert_eq!(
            ReplayState::from_status(Some(TransitionStatus::Pending)),
            ReplayState::InFlight
        );
        assert_eq!(
            ReplayState::from_status(Some(TransitionStatus::Failed)),
            ReplayState::FailedPreviously
        );
    }
}

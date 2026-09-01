//! Deterministic `transition_id` derived from trusted workflow state.
//!
//! The id is the first **32 lowercase hex characters** (128 bits) of
//! `SHA-256(workflow_id | workflow_revision | current_stage | role | result)`.
//!
//! # Why derived, never supplied
//!
//! `transition_id` is part of the *trusted* vocabulary. The LLM never
//! authors it; OpenAB always computes it from the trusted assignment
//! plus the trusted claim. This makes the id:
//!
//! - **Deterministic** — the same tuple always produces the same id.
//! - **Replay-detectable** — duplicate `(workflow_id, revision, stage,
//!   role, result)` tuples hit the same id; the ledger returns
//!   `ALREADY_DELIVERED` (Phase 2+).
//! - **Cycle-stable** — a new round after `workflow_revision` increments
//!   produces a different id for the same `(stage, role, result)`, which
//!   is how `PRIMARY_CORRECTION_PENDING → PRIMARY_COMPLETE →
//!   VERIFIER_ACTIVE` is distinguishable from the first
//!   `PRIMARY_COMPLETE → VERIFIER_ACTIVE`.
//!
//! # Why 128 bits
//!
//! 64 bits is technically enough to avoid accidental collision, but the
//! id is a workflow-control token and there is no meaningful cost to
//! using the full 128 bits the SHA-256 prefix already gives us.
//!
//! [`CompletionResult`]: super::state::CompletionResult
//! [`WorkflowRole`]: super::state::WorkflowRole
//! [`WorkflowStage`]: super::state::WorkflowStage

use sha2::{Digest, Sha256};

use super::state::{CompletionResult, WorkflowRole, WorkflowStage};

/// Number of bytes (and therefore hex chars) returned by
/// [`derive_transition_id`]. Pinned at 16 bytes / 32 hex chars.
pub const TRANSITION_ID_HEX_CHARS: usize = 32;

/// Compute the deterministic transition id from trusted workflow inputs.
///
/// Output is exactly 32 lowercase hex characters (128 bits). The
/// `workflow_id`, `workflow_revision`, `current_stage`, `role`, and
/// `result` are joined with the ASCII `|` separator and hashed with
/// SHA-256; the first 16 bytes (128 bits) of the digest are emitted
/// as lowercase hex.
///
/// Determinism: identical inputs produce byte-identical output. There
/// is no randomness, no clock dependency, and no I/O.
pub fn derive_transition_id(
    workflow_id: &str,
    workflow_revision: u64,
    current_stage: WorkflowStage,
    role: WorkflowRole,
    result: CompletionResult,
) -> String {
    let payload = format!("{workflow_id}|{workflow_revision}|{current_stage}|{role}|{result}");
    let digest = Sha256::digest(payload.as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::state::{CompletionResult, WorkflowRole, WorkflowStage};

    // ---- Test 16: same trusted input → same ID ----

    #[test]
    fn same_inputs_produce_same_id() {
        let a = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        let b = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        assert_eq!(a, b);
    }

    // ---- Test 17: revision changes → different ID ----

    #[test]
    fn revision_change_produces_different_id() {
        let rev0 = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        let rev1 = derive_transition_id(
            "wf-001",
            1,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        assert_ne!(
            rev0, rev1,
            "incrementing workflow_revision must change the id"
        );
    }

    // ---- Test 18: stage changes → different ID ----

    #[test]
    fn stage_change_produces_different_id() {
        let primary = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        let correction = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryCorrectionPending,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        assert_ne!(
            primary, correction,
            "different current_stage must produce different ids"
        );
    }

    // ---- Test 19: role / result changes → different ID ----

    #[test]
    fn role_and_result_change_produce_different_id() {
        let baseline = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::VerifierActive,
            WorkflowRole::Verifier,
            CompletionResult::Pass,
        );
        let role_changed = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::VerifierActive,
            WorkflowRole::Primary,
            CompletionResult::Pass,
        );
        let result_changed = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::VerifierActive,
            WorkflowRole::Verifier,
            CompletionResult::Fail,
        );
        assert_ne!(baseline, role_changed, "role change must change id");
        assert_ne!(baseline, result_changed, "result change must change id");
        assert_ne!(
            role_changed, result_changed,
            "role-only and result-only changes must produce distinct ids"
        );
    }

    // ---- Test 20: output length is 32 hex chars (128-bit prefix) ----

    #[test]
    fn output_is_32_lowercase_hex_chars() {
        let id = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        assert_eq!(id.len(), TRANSITION_ID_HEX_CHARS);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "id must be lowercase hex only, got {id:?}"
        );
    }

    #[test]
    fn output_stability_against_known_vector() {
        // Locked test vector — changing the SHA-256 input format would
        // silently change every transition id in flight, so this pins
        // the exact format. Generated from:
        //   SHA-256("wf-001|0|PRIMARY_ACTIVE|PRIMARY|COMPLETE")[:16]
        let id = derive_transition_id(
            "wf-001",
            0,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        );
        // 128-bit hex prefix of the SHA-256 digest of the payload
        // "wf-001|0|PRIMARY_ACTIVE|PRIMARY|COMPLETE". Computed once and
        // pinned here so accidental changes to the input format break
        // the test loudly.
        let expected = "2c2bf05d5815f8b8a914fbbe02e23d4b";
        assert_eq!(
            id, expected,
            "transition id drifted — input format changed?"
        );
    }
}

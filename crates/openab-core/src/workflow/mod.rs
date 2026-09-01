//! OpenAB-native three-agent coding workflow state machine and
//! persistence (workflow `20260818-openab-automatic-three-agent-handoff`).
//!
//! OpenAB is the sole workflow authority for this flow. The legacy
//! `OpenAB → AAP completion bridge` and ai-workstation's
//! `.agents/workflow_assignment.json` are explicitly out of scope;
//! Phase 1+ ships the new, OpenAB-owned vocabulary and persistence
//! without disturbing either of them.
//!
//! # Phase 1 surface
//!
//! - [`state`] — the canonical six stages, three roles, three results,
//!   three modes, plus pure helpers
//!   ([`state::expected_role_for_stage`],
//!   [`state::legal_next_stage`]) and the terminal-state predicate.
//! - [`assignment`] — the `v2` schema, fail-closed load, atomic save,
//!   and the `<project_root>/.openab/workflow_assignment.json`
//!   persistence contract.
//! - [`transition_id`] — deterministic 128-bit SHA-256 prefix id
//!   derived from trusted state, never supplied by the agent.
//!
//! # Phase 2 surface
//!
//! - [`completion`] — the untrusted `<role_completion>` block parser.
//!   Plain-text `VERIFIER_PASS` / `HANDOFF` / `@ArthurCodex` does
//!   NOT count. Multiple blocks → `AMBIGUOUS_MULTIPLE_CLAIMS`. The
//!   parsed [`completion::ParsedClaim`] is still untrusted — the
//!   validator is the only thing that may advance state.
//! - [`validator`] — the trusted 10-check validator. Every reject is
//!   tagged with a stable reason token from [`validator::reason`]
//!   so audit logs and downstream code can match on it.
//! - [`ledger`] — the project-local transition ledger at
//!   `<project_root>/.openab/workflow_transitions.json`. Four-state
//!   lifecycle (`RESERVED` / `PENDING` / `DELIVERED` / `FAILED`),
//!   bounded at 256 rows, fail-closed on malformed JSON.
//!
//! # What is NOT here yet
//!
//! Later phases add the `<workflow_context>` injector (now in Phase 3),
//! the targeted Discord handoff trigger (Phase 4), the workflow-role
//! gate (now in Phase 3), the `WorkflowService` orchestrator (Phase 4),
//! and degraded-mode routing (Phase 4+).

pub mod assignment;
pub mod completion;
pub mod context;
pub mod handoff;
pub mod identity;
pub mod ledger;
pub mod recovery;
pub mod service;
pub mod state;
pub mod transition_id;
pub mod validator;

pub use assignment::{
    assignment_path, load_assignment, save_assignment_atomic, AssignmentError, WorkflowAssignment,
    ASSIGNMENT_FILENAME, SCHEMA_VERSION, SUPPORTED_DEFECT_LOOP_MAX, WORKFLOW_DIR,
};
pub use completion::{parse_role_completion, ParseOutcome, ParsedClaim};
pub use context::{
    build_workflow_context, decide_context_injection, decide_workflow_gate,
    is_tech_lead_authorized, parse_sender_user_id_from_json, phase3_a13_decide,
    render_workflow_context_block, ContextDecision, ContextReason, GateDecision, GateReason,
    SenderIdentity, WorkflowContext,
};
pub use handoff::{
    render_activation_body, render_role_completion_contract, ChatAdapterWorkflowMessenger,
    MessengerError, WorkflowMessenger,
};
pub use identity::{
    current_agent_identity_from_env, resolve_role_from_assignment, AgentIdentity, IdentityError,
    RoleResolution, ARTHUR_AGENT_NAME_ENV,
};
pub use ledger::{
    LedgerEntry, LedgerError, TransitionLedger, TransitionStatus, LEDGER_FILENAME,
    MAX_LEDGER_ENTRIES,
};
pub use recovery::{reconcile_project_workflow, RecoveryError, RecoveryOutcome, RecoveryReport};
pub use service::{TurnOutcome, WorkflowService};
pub use state::{
    expected_role_for_stage, legal_next_stage, CompletionResult, WorkflowMode, WorkflowRole,
    WorkflowStage,
};
pub use transition_id::{derive_transition_id, TRANSITION_ID_HEX_CHARS};
pub use validator::{validate, RejectReason, ReplayState, ValidationOutcome};

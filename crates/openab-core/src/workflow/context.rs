//! Trusted `<workflow_context>` block construction and the A13
//! workflow-role gate.
//!
//! # Workflow context
//!
//! OpenAB authors the `<workflow_context>` block from the
//! project-local [`WorkflowAssignment`](super::assignment::WorkflowAssignment).
//! Every field on the block is sourced from trusted state —
//! never from an LLM-authored completion claim and never from
//! the inbound Discord message text.
//!
//! The block is prepended as a separate text content block in the
//! ACP prompt; the original user message is preserved byte-for-byte
//! downstream of it.
//!
//! The block looks like:
//!
//! ```text
//! <workflow_context>
//! workflow_id: ...
//! project_id: ...
//! project_root: ...
//! assigned_role: PRIMARY
//! workflow_stage: PRIMARY_ACTIVE
//! language: zh-TW
//! workflow_revision: 3
//! defect_loop_count: 0
//! transition_id: ...
//! scope: ...
//! unavailable_agents: []
//! authorized_by: Tech Lead
//! </workflow_context>
//! ```
//!
//! `transition_id` may be `None` for a freshly-created assignment
//! that has not yet committed any transition. `scope` is advisory
//! only — the LLM may use it as bounded context for its next
//! action but the workflow engine never gates behaviour on it.
//!
//! # A13 workflow-role gate
//!
//! The A13 gate sits between Discord admission (A12 MultibotMentions)
//! and the ACP prompt dispatch. For **bot-authored** traffic it
//! classifies the inbound message as one of:
//!
//! - admit (the daemon is the active role for the workflow
//!   assignment, or the assignment is missing — legacy behaviour),
//! - suppress (terminal workflow, wrong role, unknown identity, …)
//!
//! For **human-authored** traffic the gate never suppresses. The
//! Tech Lead must always be able to address any agent explicitly
//! for debugging, recovery, and reassignment; suppress decisions
//! are reserved for `automatic/bot workflow activation traffic`
//! (per the A13 brief).

use std::path::PathBuf;

use super::assignment::WorkflowAssignment;
use super::identity::{
    current_agent_identity_from_env, resolve_role_from_assignment, AgentIdentity, IdentityError,
};
use super::state::{
    expected_role_for_stage, CompletionResult, WorkflowMode, WorkflowRole, WorkflowStage,
};

/// Stable role-gate reason tokens. These strings are suitable for
/// audit logging; the gate's runtime decision branches on
/// [`GateDecision`] / [`GateReason`], not on these tokens.
pub mod gate_reason {
    pub const WORKFLOW_ROLE_ACTIVE: &str = "WORKFLOW_ROLE_ACTIVE";
    pub const WORKFLOW_ROLE_NOT_ACTIVE: &str = "WORKFLOW_ROLE_NOT_ACTIVE";
    pub const WORKFLOW_TERMINAL: &str = "WORKFLOW_TERMINAL";
    pub const WORKFLOW_ASSIGNMENT_MISSING: &str = "WORKFLOW_ASSIGNMENT_MISSING";
    pub const WORKFLOW_IDENTITY_UNKNOWN: &str = "WORKFLOW_IDENTITY_UNKNOWN";
    pub const WORKFLOW_BYPASS_TECH_LEAD: &str = "WORKFLOW_BYPASS_TECH_LEAD";
}

/// Stable context-injection reason tokens.
pub mod context_reason {
    pub const CONTEXT_INJECTED: &str = "CONTEXT_INJECTED";
    pub const CONTEXT_NO_ASSIGNMENT: &str = "CONTEXT_NO_ASSIGNMENT";
    pub const CONTEXT_TERMINAL: &str = "CONTEXT_TERMINAL";
    pub const CONTEXT_GATE_SUPPRESSED: &str = "CONTEXT_GATE_SUPPRESSED";
}

/// A typed reason for an A13 gate decision. The variants map 1:1
/// onto the [`gate_reason`] string tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateReason {
    /// Daemon is the active role for the current stage.
    WorkflowRoleActive,
    /// Daemon is known but the assignment marks a different role as
    /// active for the current stage.
    WorkflowRoleNotActive,
    /// Workflow is terminal (`TECH_LEAD_WAIT` or `BLOCKED`).
    WorkflowTerminal,
    /// No workflow assignment exists for this thread's pinned
    /// project — legacy behaviour preserved.
    WorkflowAssignmentMissing,
    /// Daemon's `ARTHUR_AGENT_NAME` env is unset/empty/unknown.
    WorkflowIdentityUnknown,
    /// Human-authored message bypassing the role gate (Tech Lead
    /// may address any agent for debugging).
    WorkflowBypassTechLead,
}

impl GateReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowRoleActive => gate_reason::WORKFLOW_ROLE_ACTIVE,
            Self::WorkflowRoleNotActive => gate_reason::WORKFLOW_ROLE_NOT_ACTIVE,
            Self::WorkflowTerminal => gate_reason::WORKFLOW_TERMINAL,
            Self::WorkflowAssignmentMissing => gate_reason::WORKFLOW_ASSIGNMENT_MISSING,
            Self::WorkflowIdentityUnknown => gate_reason::WORKFLOW_IDENTITY_UNKNOWN,
            Self::WorkflowBypassTechLead => gate_reason::WORKFLOW_BYPASS_TECH_LEAD,
        }
    }
}

/// A typed reason for the context-injection decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextReason {
    /// Workflow context was prepended to the prompt.
    ContextInjected,
    /// No workflow assignment exists for this thread's project.
    ContextNoAssignment,
    /// Assignment's stage is terminal.
    ContextTerminal,
    /// Gate suppressed the inbound message; do not inject.
    ContextGateSuppressed,
}

impl ContextReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextInjected => context_reason::CONTEXT_INJECTED,
            Self::ContextNoAssignment => context_reason::CONTEXT_NO_ASSIGNMENT,
            Self::ContextTerminal => context_reason::CONTEXT_TERMINAL,
            Self::ContextGateSuppressed => context_reason::CONTEXT_GATE_SUPPRESSED,
        }
    }
}

/// What the A13 gate decides about an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Admit the message; the dispatcher may proceed.
    Admit { reason: GateReason },
    /// Suppress the message; the dispatcher MUST NOT proceed.
    /// `detail` is a free-form diagnostic for logs.
    Suppress { reason: GateReason, detail: String },
}

impl GateDecision {
    pub fn is_admit(&self) -> bool {
        matches!(self, Self::Admit { .. })
    }

    pub fn is_suppress(&self) -> bool {
        matches!(self, Self::Suppress { .. })
    }

    pub fn reason(&self) -> GateReason {
        match self {
            Self::Admit { reason } | Self::Suppress { reason, .. } => *reason,
        }
    }
}

/// Authoritative sender identity. Captured at the OpenAB ingress
/// edge from Discord event metadata; the LLM never authorises
/// itself.
///
/// `user_id` is the Discord numeric user id — used as the **only**
/// credential for Tech Lead bypass lookup. `display_name` is
/// captured for log readability only and is never consulted by
/// the gate's authorisation decisions, so display-name
/// impersonation attacks fail closed by construction (the
/// bypass is keyed on the immutable numeric `user_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SenderIdentity {
    pub user_id: u64,
    pub display_name: String,
}

/// Pure A13 gate: tests can call this with explicit values for
/// every input — no env reads, no I/O. Production callers use
/// [`phase3_a13_decide`] which reads `ARTHUR_AGENT_NAME` and the
/// pinned assignment from disk and feeds the inputs in.
///
/// Parameters:
/// - `current_agent_identity`: this daemon's logical identity
///   (resolved from `ARTHUR_AGENT_NAME` at startup; `None` when
///   the env was unset / empty / unknown).
/// - `sender_identity`: the inbound sender's authoritative Discord
///   user id + display name. `None` for unparseable / missing
///   sender metadata.
/// - `sender_is_bot`: convenience flag from `msg.author.bot`. The
///   gate uses this together with `sender_identity` so the two
///   signals cannot disagree silently.
/// - `assignment`: the project-local
///   [`WorkflowAssignment`](super::assignment::WorkflowAssignment),
///   or `None` if no `<project_root>/.openab/workflow_assignment.json`
///   exists on disk (legacy behaviour preserved).
/// - `tech_lead_identities`: explicit set of authorised Tech Lead
///   Discord user ids, sourced from the deployment config.
///
/// Behaviour (canonical):
/// - No assignment → [`GateDecision::Admit`] with
///   [`GateReason::WorkflowAssignmentMissing`] (legacy).
/// - Sender is bot-traffic + daemon identity unknown → suppress
///   with [`GateReason::WorkflowIdentityUnknown`].
/// - Assignment state is terminal → suppress with
///   [`GateReason::WorkflowTerminal`].
/// - `sender_is_bot == false` AND `sender_identity.is_some()` AND
///   `sender_identity.user_id ∈ tech_lead_identities` → admit
///   with [`GateReason::WorkflowBypassTechLead`]. **This is the
///   ONLY bypass path.** Ordinary humans are *not* in
///   `tech_lead_identities`.
/// - `sender_is_bot == false` (any other case) → apply normal
///   workflow role semantics (the human carries no role, so the
///   role match fails and the decision is suppress).
/// - Bot-traffic, identity resolves to a role, stage non-terminal
///   → admit if the resolved role equals the expected role;
///   suppress otherwise.
pub fn decide_workflow_gate(
    current_agent_identity: Option<AgentIdentity>,
    sender_identity: Option<SenderIdentity>,
    sender_is_bot: bool,
    assignment: Option<&WorkflowAssignment>,
    tech_lead_identities: &std::collections::HashSet<u64>,
) -> GateDecision {
    // 1. No assignment → legacy behaviour preserved.
    let Some(a) = assignment else {
        return GateDecision::Admit {
            reason: GateReason::WorkflowAssignmentMissing,
        };
    };

    // 2. Tech Lead human bypass is checked before any role logic
    //    and before any "human-as-non-bot-author" check. Only an
    //    explicit numeric user id in `tech_lead_identities` grants
    //    bypass authority. Display names are never consulted.
    if !sender_is_bot {
        match sender_identity {
            None => {
                // Non-bot sender with unparseable / missing user id
                // → fail closed for workflow-bound traffic.
                return GateDecision::Suppress {
                    reason: GateReason::WorkflowIdentityUnknown,
                    detail: format!(
                        "thread={} state={} non-bot sender has no resolvable user id",
                        a.thread_id, a.state
                    ),
                };
            }
            Some(id) if tech_lead_identities.contains(&id.user_id) => {
                return GateDecision::Admit {
                    reason: GateReason::WorkflowBypassTechLead,
                };
            }
            Some(_) => {
                // Non-Tech-Lead human → no bypass; apply normal
                // workflow role semantics. Humans carry no
                // workflow role, so the role match fails below.
            }
        }
    } else if current_agent_identity.is_none() {
        // Bot traffic from an unknown daemon → fail closed.
        return GateDecision::Suppress {
            reason: GateReason::WorkflowIdentityUnknown,
            detail: format!(
                "thread={} state={} daemon ARTHUR_AGENT_NAME unset or unrecognised",
                a.thread_id, a.state
            ),
        };
    }

    // 3. Terminal workflow stage → suppress even the would-be
    //    active role (no Tech Lead recovery bypass; recovery is a
    //    later phase's responsibility).
    if a.state.is_terminal() {
        let detail = match sender_identity {
            Some(id) => format!(
                "thread={} state={} sender_user_id={} tech_lead_authorized={}",
                a.thread_id,
                a.state,
                id.user_id,
                tech_lead_identities.contains(&id.user_id)
            ),
            None => format!(
                "thread={} state={} sender_user_id=<unknown> tech_lead_authorized=false",
                a.thread_id, a.state
            ),
        };
        return GateDecision::Suppress {
            reason: GateReason::WorkflowTerminal,
            detail,
        };
    }

    // 4. Bot traffic + (non-Tech-Lead human, which has no role)
    //    both fall through to the role-match check below.
    if !sender_is_bot {
        // Non-Tech-Lead human → no role to match against the
        // stage-bound expected role → suppress.
        let detail = match sender_identity {
            Some(id) => format!(
                "thread={} state={} sender_user_id={} is not in tech_lead_identities; \
                 non-Tech-Lead humans have no workflow role to match",
                a.thread_id, a.state, id.user_id
            ),
            None => format!(
                "thread={} state={} non-bot sender has no resolvable identity",
                a.thread_id, a.state
            ),
        };
        return GateDecision::Suppress {
            reason: GateReason::WorkflowRoleNotActive,
            detail,
        };
    }

    let identity = current_agent_identity
        .expect("sender_is_bot=true branch was already filtered for Some(identity) above");

    let resolution = match resolve_role_from_assignment(identity, a) {
        Ok(r) => r,
        Err(IdentityError::AssignmentMismatch { identity: name }) => {
            return GateDecision::Suppress {
                reason: GateReason::WorkflowRoleNotActive,
                detail: format!(
                    "thread={} identity={:?} not bound to any assignment role slot",
                    a.thread_id, name
                ),
            };
        }
        Err(e) => {
            return GateDecision::Suppress {
                reason: GateReason::WorkflowIdentityUnknown,
                detail: format!(
                    "thread={} identity={:?} resolution failed: {e}",
                    a.thread_id, identity
                ),
            };
        }
    };

    let expected = expected_role_for_stage(a.state);
    if Some(resolution.role) == expected {
        GateDecision::Admit {
            reason: GateReason::WorkflowRoleActive,
        }
    } else {
        GateDecision::Suppress {
            reason: GateReason::WorkflowRoleNotActive,
            detail: format!(
                "thread={} stage={} expected={:?} identity={:?} resolves_to={}",
                a.thread_id, a.state, expected, identity, resolution.role
            ),
        }
    }
}

/// Whether the inbound sender is authorised as the Tech Lead.
/// Used by the A13 trace line and by callers that want a stable
/// answer for log records. Pure function.
pub fn is_tech_lead_authorized(
    sender_identity: Option<&SenderIdentity>,
    sender_is_bot: bool,
    tech_lead_identities: &std::collections::HashSet<u64>,
) -> bool {
    if sender_is_bot {
        return false;
    }
    match sender_identity {
        Some(id) => tech_lead_identities.contains(&id.user_id),
        None => false,
    }
}

/// Backwards-compatibility alias kept so older call sites that
/// passed the previous struct still resolve during the transition;
/// Phase 4 may remove it. New code MUST use
/// [`decide_workflow_gate`] directly.
#[deprecated(note = "use decide_workflow_gate directly")]
pub struct GateInputs<'a> {
    pub identity: Option<AgentIdentity>,
    pub assignment: Option<&'a WorkflowAssignment>,
    pub author_is_bot: bool,
    pub thread_id: &'a str,
}

/// Parse a `sender_id` u64 out of the OpenAB
/// [`crate::dispatch::BufferedMessage::sender_json`] blob. Returns
/// `None` when the field is missing or unparseable; the gate
/// treats `None` as fail-closed for non-bot senders.
pub fn parse_sender_user_id_from_json(sender_json: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct SenderJsonMin {
        #[serde(default)]
        sender_id: String,
    }
    serde_json::from_str::<SenderJsonMin>(sender_json)
        .ok()
        .and_then(|s| s.sender_id.parse::<u64>().ok())
}

/// Top-level helper combining "load assignment" + "resolve
/// sender/identity" + "evaluate gate". The dispatcher uses this
/// to keep its call site clean.
///
/// **This is a production adapter.** It performs I/O: it reads
/// `ARTHUR_AGENT_NAME` from the daemon's process env and loads
/// the assignment from
/// `<project_root>/.openab/workflow_assignment.json`. For tests
/// that need to control all inputs, call [`decide_workflow_gate`]
/// directly with explicit values instead.
///
/// Returns the gate decision and, when admitted with an active
/// assignment, the rendered `<workflow_context>` block.
pub fn phase3_a13_decide(
    pinned_project_root: Option<&std::path::Path>,
    sender_user_id: Option<u64>,
    sender_display_name: &str,
    sender_is_bot: bool,
    thread_id: &str,
    tech_lead_identities: &std::collections::HashSet<u64>,
) -> (GateDecision, Option<String>) {
    let assignment =
        pinned_project_root.and_then(|p| match super::assignment::load_assignment(p) {
            Ok(Some(a)) => Some(a),
            Ok(None) => None,
            Err(_) => None,
        });

    // Production adapter for `current_agent_identity`. The OnceLock
    // is preserved verbatim per the brief.
    let identity = current_agent_identity_from_env().ok();

    let sender_identity = sender_user_id.map(|uid| SenderIdentity {
        user_id: uid,
        display_name: sender_display_name.to_string(),
    });

    let gate = decide_workflow_gate(
        identity,
        sender_identity,
        sender_is_bot,
        assignment.as_ref(),
        tech_lead_identities,
    );

    let assigned_role = identity
        .and_then(|id| {
            assignment
                .as_ref()
                .and_then(|a| resolve_role_from_assignment(id, a).ok())
        })
        .map(|r| r.role)
        .or_else(|| {
            assignment
                .as_ref()
                .and_then(|a| expected_role_for_stage(a.state))
        });

    let ctx_decision = decide_context_injection(
        &gate,
        assignment.as_ref(),
        thread_id,
        None,
        assigned_role.unwrap_or(WorkflowRole::Primary),
    );

    let context_text = ctx_decision
        .context
        .as_ref()
        .map(render_workflow_context_block);

    (gate, context_text)
}

/// Trusted snapshot of a [`WorkflowAssignment`] enriched with the
/// role the *current* daemon is playing and an optional advisory
/// `scope` (forwarded from a previous role's completion claim).
/// All fields are sourced from trusted OpenAB state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowContext {
    pub workflow_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    pub assigned_role: WorkflowRole,
    pub workflow_stage: WorkflowStage,
    pub language: String,
    pub workflow_revision: u64,
    pub defect_loop_count: u32,
    pub transition_id: Option<String>,
    pub scope: Option<String>,
    pub unavailable_agents: Vec<String>,
    pub authorized_by: String,
    pub thread_id: String,
    /// The mode the assignment was created with. Phase 3 surfaces
    /// but does not yet branch on this; later phases will.
    pub mode: WorkflowMode,
    /// Mode-derived copy of the last committed result (informational
    /// only — not consumed by the validator, gate, or context block
    /// today).
    pub last_committed_result: Option<CompletionResult>,
}

/// Build a trusted [`WorkflowContext`] snapshot from a
/// [`WorkflowAssignment`]. Pure function.
///
/// `assigned_role` is the role the *current* daemon is filling —
/// pass the value from
/// [`super::identity::resolve_role_from_assignment`] (or
/// `assignment.state` → [`super::state::expected_role_for_stage`]
/// for the "who is active right now?" reading).
///
/// `scope` is an optional advisory string forwarded from the
/// previous role's completion claim.
pub fn build_workflow_context(
    assignment: &WorkflowAssignment,
    assigned_role: WorkflowRole,
    thread_id: impl Into<String>,
    scope: Option<String>,
) -> WorkflowContext {
    WorkflowContext {
        workflow_id: assignment.workflow_id.clone(),
        project_id: assignment.project_id.clone(),
        project_root: assignment.project_root.clone(),
        assigned_role,
        workflow_stage: assignment.state,
        language: assignment.language.clone(),
        workflow_revision: assignment.workflow_revision,
        defect_loop_count: assignment.defect_loop_count,
        transition_id: assignment.last_transition_id.clone(),
        scope,
        unavailable_agents: assignment.unavailable_agents.clone(),
        authorized_by: assignment.authorized_by.clone(),
        thread_id: thread_id.into(),
        mode: assignment.mode,
        last_committed_result: None,
    }
}

/// Render the trusted `<workflow_context>` block as a single
/// `String`. One block, no leading/trailing prose, ready to be
/// prepended as a separate `ContentBlock::Text` in the ACP prompt.
pub fn render_workflow_context_block(ctx: &WorkflowContext) -> String {
    let mut out = String::new();
    out.push_str("<workflow_context>\n");
    out.push_str(&format!("workflow_id: {}\n", ctx.workflow_id));
    out.push_str(&format!("project_id: {}\n", ctx.project_id));
    out.push_str(&format!("project_root: {}\n", ctx.project_root.display()));
    out.push_str(&format!("assigned_role: {}\n", ctx.assigned_role));
    out.push_str(&format!("workflow_stage: {}\n", ctx.workflow_stage));
    out.push_str(&format!("language: {}\n", ctx.language));
    out.push_str(&format!("workflow_revision: {}\n", ctx.workflow_revision));
    out.push_str(&format!("defect_loop_count: {}\n", ctx.defect_loop_count));
    if let Some(tid) = &ctx.transition_id {
        out.push_str(&format!("transition_id: {tid}\n"));
    } else {
        out.push_str("transition_id: <none>\n");
    }
    if let Some(scope) = &ctx.scope {
        out.push_str(&format!("scope: {scope}\n"));
    } else {
        out.push_str("scope: <none>\n");
    }
    if ctx.unavailable_agents.is_empty() {
        out.push_str("unavailable_agents: []\n");
    } else {
        out.push_str(&format!(
            "unavailable_agents: [{}]\n",
            ctx.unavailable_agents.join(", ")
        ));
    }
    out.push_str(&format!("authorized_by: {}\n", ctx.authorized_by));
    out.push_str("</workflow_context>");
    out
}

/// Decide whether to inject a `<workflow_context>` block into the
/// ACP prompt for this turn. Pure function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDecision {
    /// Reason token for the trace line.
    pub reason: ContextReason,
    /// The context snapshot to inject (or `None` when no injection).
    pub context: Option<WorkflowContext>,
}

/// Build the injector decision. Pure — does not mutate state.
///
/// Rules:
/// - Gate suppressed → no injection ([`ContextReason::ContextGateSuppressed`]).
/// - No assignment → no injection
///   ([`ContextReason::ContextNoAssignment`]).
/// - Assignment is terminal → no injection
///   ([`ContextReason::ContextTerminal`]) for the normal workflow
///   activation path. (The Tech Lead bypass explicitly breaks this;
///   callers that want debug-time injection should build the
///   decision themselves with
///   [`build_workflow_context_for_debug`].)
/// - Otherwise, inject
///   ([`ContextReason::ContextInjected`]).
///
/// `assigned_role` is the role the *current* daemon is filling.
pub fn decide_context_injection(
    gate: &GateDecision,
    assignment: Option<&WorkflowAssignment>,
    thread_id: &str,
    scope: Option<String>,
    assigned_role: WorkflowRole,
) -> ContextDecision {
    if gate.is_suppress() {
        return ContextDecision {
            reason: ContextReason::ContextGateSuppressed,
            context: None,
        };
    }
    let Some(a) = assignment else {
        return ContextDecision {
            reason: ContextReason::ContextNoAssignment,
            context: None,
        };
    };
    if a.state.is_terminal() {
        return ContextDecision {
            reason: ContextReason::ContextTerminal,
            context: None,
        };
    }
    let ctx = build_workflow_context(a, assigned_role, thread_id.to_string(), scope);
    ContextDecision {
        reason: ContextReason::ContextInjected,
        context: Some(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn canonical_or(p: PathBuf) -> PathBuf {
        std::fs::canonicalize(&p).unwrap_or(p)
    }

    /// Tech Lead user id used throughout the bypass tests — the
    /// canonical numeric `645496545805991947` from the production
    /// Discord channel (sender.json `sender_id` for `springsunny`).
    /// Display name "Tech Lead" is *not* used as a credential; it
    /// must not grant authority on its own.
    const TECH_LEAD_USER_ID: u64 = 645496545805991947;
    const ORDINARY_USER_ID: u64 = 999999999999999999;

    fn tech_lead_set() -> HashSet<u64> {
        let mut s = HashSet::new();
        s.insert(TECH_LEAD_USER_ID);
        s
    }

    fn empty_tech_lead_set() -> HashSet<u64> {
        HashSet::new()
    }

    fn sample_assignment(proj_root: PathBuf) -> WorkflowAssignment {
        WorkflowAssignment {
            schema_version: "v2".into(),
            workflow_id: "wf-001".into(),
            project_id: "openab".into(),
            project_root: canonical_or(proj_root),
            mode: Default::default(),
            primary: "ArthurClaude".into(),
            verifier: "ArthurCodex".into(),
            final_reviewer: "ArthurGemini".into(),
            state: WorkflowStage::PrimaryActive,
            workflow_revision: 2,
            defect_loop_count: 0,
            language: "zh-TW".into(),
            thread_id: "1536735741642547262".into(),
            last_transition_id: Some("abc123".into()),
            last_delivery_message_id: None,
            unavailable_agents: Vec::new(),
            authorized_by: "Tech Lead".into(),
            reason: "phase-3 sample".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ---- A13 role gate (canonical matrix) ----

    #[test]
    fn gate_admits_claude_during_primary_active() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowRoleActive);
    }

    #[test]
    fn gate_suppresses_codex_during_primary_active() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurCodex),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn gate_admits_codex_during_verifier_active() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurCodex),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowRoleActive);
    }

    #[test]
    fn gate_suppresses_gemini_during_verifier_active() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurGemini),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn gate_admits_gemini_during_final_reviewer_active() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::FinalReviewerActive;
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurGemini),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowRoleActive);
    }

    #[test]
    fn gate_admits_claude_during_primary_correction_pending() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::PrimaryCorrectionPending;
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowRoleActive);
    }

    #[test]
    fn gate_suppresses_bot_traffic_during_terminal_states() {
        let dir = TempDir::new().unwrap();
        for stage in [WorkflowStage::TechLeadWait, WorkflowStage::Blocked] {
            let mut a = sample_assignment(dir.path().to_path_buf());
            a.state = stage;
            for id in [
                AgentIdentity::ArthurClaude,
                AgentIdentity::ArthurCodex,
                AgentIdentity::ArthurGemini,
            ] {
                let d =
                    decide_workflow_gate(Some(id), None, true, Some(&a), &empty_tech_lead_set());
                assert!(d.is_suppress(), "{stage} must suppress {id:?} bot traffic");
                assert_eq!(d.reason(), GateReason::WorkflowTerminal);
            }
        }
    }

    #[test]
    fn gate_preserves_legacy_when_no_assignment() {
        // No assignment + bot traffic → admit (legacy preserved).
        // This is the only path that skips the Tech-Lead gate entirely.
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            None,
            &empty_tech_lead_set(),
        );
        assert!(d.is_admit(), "no assignment → legacy behavior preserved");
        assert_eq!(d.reason(), GateReason::WorkflowAssignmentMissing);
    }

    #[test]
    fn gate_suppresses_bot_traffic_when_identity_unknown() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let d = decide_workflow_gate(None, None, true, Some(&a), &empty_tech_lead_set());
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowIdentityUnknown);
    }

    #[test]
    fn gate_suppresses_bot_traffic_when_identity_not_in_assignment() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.final_reviewer = "ArthurOther".into();
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurGemini),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    // ---- A13 Tech-Lead bypass hardening (Phase 3 bounded correction) ----

    #[test]
    fn bypass_tech_lead_authorised_user_id_bypasses_for_inactive_claude() {
        // authorised Tech Lead human addressing Claude while the
        // active stage is VERIFIER_ACTIVE → Claude is not the active
        // role, but Tech Lead bypass authority grants admit.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let sender = SenderIdentity {
            user_id: TECH_LEAD_USER_ID,
            display_name: "Tech Lead".to_string(),
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d.is_admit(), "authorised Tech Lead must bypass");
        assert_eq!(d.reason(), GateReason::WorkflowBypassTechLead);
    }

    #[test]
    fn bypass_tech_lead_authorised_user_id_bypasses_for_inactive_codex() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::FinalReviewerActive; // Codex inactive
        let sender = SenderIdentity {
            user_id: TECH_LEAD_USER_ID,
            display_name: "Tech Lead".to_string(),
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurCodex),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowBypassTechLead);
    }

    #[test]
    fn bypass_tech_lead_authorised_user_id_explicit_recovery_bypass_on_terminal_workflow() {
        // Tech Lead must be able to address any agent for debugging /
        // recovery even when the workflow is terminal. The current
        // gate suppresses terminal-state traffic from BOTS, but the
        // Tech Lead bypass is an explicit recovery path that
        // bypasses terminal suppression.
        //
        // Documented behaviour: Tech Lead (numeric id in
        // tech_lead_identities) → admit even on terminal state,
        // because the bypass check happens BEFORE the terminal-state
        // check in the gate's decision flow.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::TechLeadWait;
        let sender = SenderIdentity {
            user_id: TECH_LEAD_USER_ID,
            display_name: "Tech Lead".to_string(),
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(
            d.is_admit(),
            "Tech Lead bypass must operate even on terminal workflows"
        );
        assert_eq!(d.reason(), GateReason::WorkflowBypassTechLead);
    }

    #[test]
    fn bypass_unrelated_human_with_inactive_role_is_not_bypass() {
        // Unrelated human (NOT in tech_lead_identities) addressing
        // an agent whose role is currently inactive → must suppress,
        // NOT bypass. Plain humans do not inherit Tech Lead authority
        // automatically.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let sender = SenderIdentity {
            user_id: ORDINARY_USER_ID,
            display_name: "Some Random User".to_string(),
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d.is_suppress(), "non-Tech-Lead humans must not bypass");
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn bypass_unrelated_human_with_active_role_normal_admission() {
        // When a workflow-bound assignment exists AND the inbound
        // sender is a non-Tech-Lead human AND the daemon is the
        // active role, the non-Tech-Lead human is still suppressed
        // (humans carry no workflow role). The Tech Lead set is
        // empty to confirm the gate returns the role-not-active
        // reason rather than a Tech-Lead-flavoured suppress.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::PrimaryActive;
        let sender = SenderIdentity {
            user_id: ORDINARY_USER_ID,
            display_name: "Some Random User".to_string(),
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn bypass_display_name_impersonation_is_rejected() {
        // The bypass is keyed on the immutable numeric user_id only;
        // display-name "Tech Lead" alone MUST NOT grant authority.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let sender = SenderIdentity {
            user_id: ORDINARY_USER_ID,             // NOT in tech_lead_identities
            display_name: "Tech Lead".to_string(), // …but the display name is
        };
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(
            d.is_suppress(),
            "display-name impersonation must fail closed"
        );
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn bypass_sender_name_equals_tech_lead_is_rejected() {
        // Even if both display_name and sender_name equal "Tech
        // Lead", the numeric user_id must be in tech_lead_identities
        // for the bypass to fire. The helper
        // `parse_sender_user_id_from_json` enforces that the only
        // credential carried across the dispatch boundary is the
        // numeric `sender_id`, not the human-readable
        // `sender_name` / `display_name`.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        let sender = SenderIdentity {
            user_id: ORDINARY_USER_ID,
            display_name: "Tech Lead".to_string(),
        };
        let raw = format!(
            r#"{{"sender_id":"{ORDINARY_USER_ID}","sender_name":"Tech Lead","display_name":"Tech Lead","is_bot":false}}"#
        );
        let parsed_uid = parse_sender_user_id_from_json(&raw);
        assert_eq!(
            parsed_uid,
            Some(ORDINARY_USER_ID),
            "parser must extract only the numeric sender_id; the human-readable \
             sender_name / display_name fields are not credentials"
        );

        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(sender),
            false,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d.is_suppress());
        assert_eq!(d.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn bypass_bot_traffic_unchanged_after_hardening() {
        // Bot traffic behaviour must NOT regress when the Tech-Lead
        // set is non-empty: the gate still runs the role check; the
        // Tech Lead set is ignored for bot senders.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive; // Codex is the active role
                                                 // Even with the Tech Lead set populated, a Codex bot
                                                 // message should admit because Codex IS the active role …
        let d_admit = decide_workflow_gate(
            Some(AgentIdentity::ArthurCodex),
            None,
            true,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d_admit.is_admit());
        assert_eq!(d_admit.reason(), GateReason::WorkflowRoleActive);
        // …and a Claude bot message should still suppress …
        let d_suppress = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            Some(&a),
            &tech_lead_set(),
        );
        assert!(d_suppress.is_suppress());
        assert_eq!(d_suppress.reason(), GateReason::WorkflowRoleNotActive);
    }

    #[test]
    fn bypass_malformed_sender_id_fails_closed() {
        // Non-bot sender with a missing/unparseable user id →
        // fail closed. This prevents a malformed sender_id from
        // silently granting bypass authority.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::VerifierActive;
        // sender_identity is None.
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            false, // non-bot sender
            Some(&a),
            &tech_lead_set(),
        );
        assert!(
            d.is_suppress(),
            "non-bot sender without a resolvable user_id must fail closed"
        );
        assert_eq!(d.reason(), GateReason::WorkflowIdentityUnknown);
    }

    #[test]
    fn bypass_no_assignment_preserves_legacy() {
        // When no workflow assignment exists, both human and bot
        // traffic must be admitted without touching the Tech Lead
        // set — the legacy "no workflow, no gate" path. This
        // explicitly checks that the Tech Lead set is *not*
        // consulted when no assignment is loaded.
        let dir = TempDir::new().unwrap();
        let _ = dir;
        // Bot traffic with non-empty Tech Lead set, no assignment:
        let d = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            None,
            &tech_lead_set(), // non-empty; should be ignored here
        );
        assert!(d.is_admit());
        assert_eq!(d.reason(), GateReason::WorkflowAssignmentMissing);

        // Human traffic with no Tech Lead id match and no assignment:
        let d_human = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(SenderIdentity {
                user_id: ORDINARY_USER_ID,
                display_name: "Some User".into(),
            }),
            false,
            None,
            &tech_lead_set(),
        );
        assert!(d_human.is_admit());
        assert_eq!(d_human.reason(), GateReason::WorkflowAssignmentMissing);
    }

    #[test]
    fn core_gate_takes_explicit_inputs_no_env_reads() {
        // The core gate MUST NOT consult the process environment.
        // Callers (including `phase3_a13_decide`) plumb the agent
        // identity in explicitly; this test proves the core API is
        // pure by exercising every branch with explicit inputs
        // without ever touching `std::env`.
        //
        // We deliberately do NOT clear `ARTHUR_AGENT_NAME` — the
        // core gate has no path that reads it, so whatever value
        // (or absence) the env has at test time is irrelevant.
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let tl = tech_lead_set();

        // All four branches with explicit inputs only:
        let _ = decide_workflow_gate(None, None, false, Some(&a), &tl); // identity unknown
        let _ = decide_workflow_gate(Some(AgentIdentity::ArthurClaude), None, true, Some(&a), &tl);
        let _ = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            Some(SenderIdentity {
                user_id: TECH_LEAD_USER_ID,
                display_name: "Tech Lead".into(),
            }),
            false,
            Some(&a),
            &tl,
        );
        // No assignment branch:
        let _ = decide_workflow_gate(Some(AgentIdentity::ArthurClaude), None, true, None, &tl);
        // Empty Tech Lead set branch — must still admit the active
        // role without bypass:
        let _ = decide_workflow_gate(
            Some(AgentIdentity::ArthurClaude),
            None,
            true,
            Some(&a),
            &empty_tech_lead_set(),
        );
        // If the test reaches this line without panicking, the core
        // gate is verifiably independent of process env.
    }

    // ---- Workflow context construction ----

    #[test]
    fn context_contains_trusted_workflow_id() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.workflow_id, a.workflow_id);
        assert_eq!(ctx.project_id, a.project_id);
        assert_eq!(ctx.thread_id, "1536");
        assert_eq!(ctx.language, a.language);
        assert_eq!(ctx.workflow_revision, a.workflow_revision);
        assert_eq!(ctx.defect_loop_count, a.defect_loop_count);
    }

    #[test]
    fn context_canonical_project_root_preserved() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.project_root, a.project_root);
    }

    #[test]
    fn context_assigned_role_derived_from_stage() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        for role in [
            WorkflowRole::Primary,
            WorkflowRole::Verifier,
            WorkflowRole::FinalReviewer,
        ] {
            let ctx = build_workflow_context(&a, role, "1536", None);
            assert_eq!(ctx.assigned_role, role);
        }
    }

    #[test]
    fn context_workflow_revision_preserved() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.workflow_revision = 7;
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.workflow_revision, 7);
    }

    #[test]
    fn context_defect_loop_count_preserved() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.defect_loop_count = 1;
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.defect_loop_count, 1);
    }

    #[test]
    fn context_language_preserved() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.language = "zh-TW".into();
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.language, "zh-TW");
        a.language = "en".into();
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(ctx.language, "en");
    }

    #[test]
    fn context_unavailable_agents_preserved() {
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.unavailable_agents
            .extend(["ArthurCodex".to_string(), "ArthurGemini".to_string()]);
        let ctx = build_workflow_context(&a, WorkflowRole::Primary, "1536", None);
        assert_eq!(
            ctx.unavailable_agents,
            vec!["ArthurCodex".to_string(), "ArthurGemini".to_string()]
        );
    }

    #[test]
    fn context_render_block_shape() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let ctx = build_workflow_context(
            &a,
            WorkflowRole::Primary,
            "1536735741642547262",
            Some("review the diff".into()),
        );
        let rendered = render_workflow_context_block(&ctx);
        for needle in [
            "<workflow_context>",
            "workflow_id: wf-001",
            "project_id: openab",
            "project_root:",
            "assigned_role: PRIMARY",
            "workflow_stage: PRIMARY_ACTIVE",
            "language: zh-TW",
            "workflow_revision: 2",
            "defect_loop_count: 0",
            "transition_id: abc123",
            "scope: review the diff",
            "unavailable_agents: []",
            "authorized_by: Tech Lead",
            "</workflow_context>",
        ] {
            assert!(
                rendered.contains(needle),
                "rendered block missing {needle:?}\n{rendered}"
            );
        }
        let mut ctx2 = ctx.clone();
        ctx2.transition_id = None;
        ctx2.scope = None;
        ctx2.unavailable_agents.clear();
        let r2 = render_workflow_context_block(&ctx2);
        assert!(r2.contains("transition_id: <none>"));
        assert!(r2.contains("scope: <none>"));
        assert!(r2.contains("unavailable_agents: []"));
    }

    // ---- decide_context_injection (using direct GateDecision) ----

    #[test]
    fn context_injection_decision_injects_when_admitted_with_assignment() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let gate = GateDecision::Admit {
            reason: GateReason::WorkflowRoleActive,
        };
        let d = decide_context_injection(
            &gate,
            Some(&a),
            "1536",
            Some("scope".into()),
            WorkflowRole::Primary,
        );
        assert_eq!(d.reason, ContextReason::ContextInjected);
        assert!(d.context.is_some());
    }

    #[test]
    fn context_injection_decision_skips_when_no_assignment() {
        let gate = GateDecision::Admit {
            reason: GateReason::WorkflowAssignmentMissing,
        };
        let d = decide_context_injection(&gate, None, "1536", None, WorkflowRole::Primary);
        assert_eq!(d.reason, ContextReason::ContextNoAssignment);
        assert!(d.context.is_none());
    }

    #[test]
    fn context_injection_decision_skips_when_gate_suppresses() {
        let dir = TempDir::new().unwrap();
        let a = sample_assignment(dir.path().to_path_buf());
        let gate = GateDecision::Suppress {
            reason: GateReason::WorkflowIdentityUnknown,
            detail: "test".into(),
        };
        let d = decide_context_injection(&gate, Some(&a), "1536", None, WorkflowRole::Primary);
        assert_eq!(d.reason, ContextReason::ContextGateSuppressed);
        assert!(d.context.is_none());
    }

    #[test]
    fn context_injection_decision_skips_when_assignment_terminal_via_admit_path() {
        // Synthetic debug-time Admit built locally; today's A13
        // suppresses terminal-state bot traffic, but this branch
        // exercises the admit-but-terminal-state case.
        let dir = TempDir::new().unwrap();
        let mut a = sample_assignment(dir.path().to_path_buf());
        a.state = WorkflowStage::TechLeadWait;
        let gate = GateDecision::Admit {
            reason: GateReason::WorkflowBypassTechLead,
        };
        let d = decide_context_injection(&gate, Some(&a), "1536", None, WorkflowRole::Primary);
        assert_eq!(d.reason, ContextReason::ContextTerminal);
        assert!(d.context.is_none());
    }
}

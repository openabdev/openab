//! Phase 4 handoff-rendering surface.
//!
//! The LLM never authors the workflow activation mention. OpenAB
//! renders a short transport/UI body and dispatches it through
//! [`WorkflowMessenger`]. The body deliberately does NOT name the
//! next agent, claim to be a "HANDOFF" envelope, or carry any
//! authoritative state. The trusted OpenAB-authored
//! `<workflow_context>` block (Phase 3) is what the recipient
//! reads; the `<workflow_activation>` block is the
//! transport/UI counterpart that wakes the next bot.
//!
//! Phase 4.2.1 also renders a `<role_completion_contract>` block
//! and appends it to every downstream activation body. The
//! contract tells the downstream agent exactly what
//! `<role_completion>` shape to emit, with every trusted field
//! pre-filled by OpenAB. The agent's only authoring choice is
//! its `result` value; OpenAB owns `transition_id`,
//! `workflow_revision`, `next_role`, `next_stage`, and any
//! recipient routing.

use std::sync::Arc;

use crate::adapter::{ChannelRef, ChatAdapter};
use crate::workflow::state::WorkflowRole;
use crate::workflow::WorkflowContext;

/// Render the trusted OpenAB-authored `<workflow_activation>`
/// block. The body carries the workflow id, project identity,
/// the recipient's assigned role and stage, the transition id,
/// and the language — everything the recipient needs to confirm
/// it is the right activation without trusting the LLM's
/// narrative.
///
/// A trusted `<role_completion_contract>` block (Phase 4.2.1) is
/// appended immediately after `</workflow_activation>`. The
/// contract enumerates the exact `<role_completion>` shape the
/// downstream agent must emit; see [`render_role_completion_contract`].
///
/// Discord mention of the recipient is added by the messenger
/// layer via `send_message_targeted(..., target_user_id)` with
/// `allowed_mentions` restricted to that one recipient. The body
/// itself does not embed `<@USER_ID>` markup.
pub fn render_activation_body(ctx: &WorkflowContext, transition_id: &str) -> String {
    let mut out = String::new();
    out.push_str("<workflow_activation>\n");
    out.push_str(&format!("workflow_id: {}\n", ctx.workflow_id));
    out.push_str(&format!("project_id: {}\n", ctx.project_id));
    out.push_str(&format!("project_root: {}\n", ctx.project_root.display()));
    out.push_str(&format!("assigned_role: {}\n", ctx.assigned_role));
    out.push_str(&format!("workflow_stage: {}\n", ctx.workflow_stage));
    out.push_str(&format!("language: {}\n", ctx.language));
    out.push_str(&format!("workflow_revision: {}\n", ctx.workflow_revision));
    out.push_str(&format!("transition_id: {transition_id}\n"));
    out.push_str("</workflow_activation>");
    // Phase 4.2.1: downstream agents were inconsistent about
    // emitting the canonical `<role_completion>` block, so the
    // trusted renderer now inlines the contract immediately after
    // the activation envelope. The contract carries the exact
    // template and the only authoring choice (`result`) the agent
    // may make.
    out.push('\n');
    out.push_str(&render_role_completion_contract(ctx));
    out
}

/// Render the trusted `<role_completion_contract>` block that
/// downstream agents MUST follow to emit their `<role_completion>`
/// claim. Every field other than `result` is pre-filled with
/// trusted workflow state; OpenAB owns the routing,
/// `transition_id`, and `workflow_revision` and the agent is
/// forbidden from authoring them.
///
/// Allowed `result` values are derived from the recipient's
/// `assigned_role`:
/// - `PRIMARY` → `COMPLETE`
/// - `VERIFIER` → `PASS | FAIL`
/// - `FINAL_REVIEWER` → `PASS | FAIL`
///
/// The block is appended to the `<workflow_activation>` body and
/// sent verbatim to the downstream agent. Plain-text
/// `HANDOFF`, plain-text `VERIFIER_PASS`, or `@NextAgent` mentions
/// outside the contract are NOT accepted — see
/// [`super::completion::parse_role_completion`] for the parser.
///
/// The contract does NOT enumerate `transition_id`,
/// `workflow_revision`, `next_role`, `next_stage`, or
/// `target_user_id` as agent-supplied fields — these tokens
/// appear only inside the contract's `forbidden_fields` list so
/// the renderer remains the only source of truth for them.
pub fn render_role_completion_contract(ctx: &WorkflowContext) -> String {
    let allowed_results = match ctx.assigned_role {
        WorkflowRole::Primary => "COMPLETE",
        WorkflowRole::Verifier | WorkflowRole::FinalReviewer => "PASS | FAIL",
    };
    let result_hint = match ctx.assigned_role {
        WorkflowRole::Primary => "<COMPLETE>",
        WorkflowRole::Verifier | WorkflowRole::FinalReviewer => "<PASS or FAIL>",
    };
    let mut out = String::new();
    out.push_str("<role_completion_contract>\n");
    out.push_str(&format!("role: {}\n", ctx.assigned_role));
    out.push_str(&format!("allowed_results: {}\n", allowed_results));
    out.push_str("output_template:\n");
    out.push_str("<role_completion>\n");
    out.push_str(&format!("role: {}\n", ctx.assigned_role));
    out.push_str(&format!("result: {result_hint}\n"));
    out.push_str(&format!("workflow_id: {}\n", ctx.workflow_id));
    out.push_str(&format!("project_id: {}\n", ctx.project_id));
    out.push_str(&format!("project_root: {}\n", ctx.project_root.display()));
    out.push_str("</role_completion>\n");
    out.push_str("forbidden_fields:\n");
    out.push_str("  - transition_id\n");
    out.push_str("  - workflow_revision\n");
    out.push_str("  - next_role\n");
    out.push_str("  - next_stage\n");
    out.push_str("  - target_user_id\n");
    out.push_str("constraints:\n");
    out.push_str("  - emit EXACTLY ONE <role_completion> block at end of final reply\n");
    out.push_str("  - do NOT output HANDOFF or any agent-routing envelope\n");
    out.push_str("  - do NOT pick the next bot or mention it; OpenAB owns transition routing\n");
    out.push_str(&format!("language: {}\n", ctx.language));
    out.push_str("</role_completion_contract>");
    out
}

/// Failure modes for [`WorkflowMessenger::send_targeted_activation`].
/// The Service uses these to distinguish "no bot delivers this
/// transition" (terminal stages) from "the platform rejected our
/// send".
#[derive(Debug)]
pub enum MessengerError {
    /// The platform accepted the request but returned no message
    /// id. Possible when the channel is muted, the bot lacks
    /// SEND, or the platform's REST endpoint contract for the
    /// send method omits the id. Treat as a hard failure for
    /// Phase 4: assignment does not advance.
    NoMessageIdReturned,
    /// The platform returned a transport-level error. The bot's
    /// HTTP client surfaces this; the Service marks the
    /// transition FAILED and does not advance the assignment.
    Transport(String),
}

impl std::fmt::Display for MessengerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMessageIdReturned => f.write_str("messenger returned no message id"),
            Self::Transport(s) => write!(f, "messenger transport error: {s}"),
        }
    }
}

impl std::error::Error for MessengerError {}

/// Production hook the `WorkflowService` uses to send exactly one
/// targeted `<workflow_activation>` message per accepted
/// transition. Implementations are responsible for
/// `allowed_mentions` being restricted to `target_user_id` so no
/// other user gets a live ping from the message.
#[async_trait::async_trait]
pub trait WorkflowMessenger: Send + Sync + 'static {
    /// Send one message containing `body` to `channel`, restricted
    /// to `target_user_id`. Returns:
    ///
    /// - `Ok(Some(message_id))` when the platform returns a stable
    ///   message id (typical Discord behaviour);
    /// - `Ok(None)` for terminal no-delivery transitions where the
    ///   Service asked the messenger to skip the send entirely
    ///   (e.g. `TECH_LEAD_WAIT`). The Service records the row in
    ///   the ledger as DELIVERED with `openab_message_id = None`;
    /// - `Err(MessengerError::NoMessageIdReturned)` /
    ///   `Err(MessengerError::Transport(..))` to fail the
    ///   transition.
    async fn send_targeted_activation(
        &self,
        channel: &ChannelRef,
        body: &str,
        target_user_id: u64,
    ) -> Result<Option<String>, MessengerError>;
}

/// Production workflow messenger backed by the platform adapter already used
/// for normal agent replies. The activation body and Discord mention are both
/// rendered here from trusted workflow state; agent output never supplies a
/// numeric recipient id.
pub struct ChatAdapterWorkflowMessenger {
    adapter: Arc<dyn ChatAdapter>,
}

impl ChatAdapterWorkflowMessenger {
    pub fn new(adapter: Arc<dyn ChatAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait::async_trait]
impl WorkflowMessenger for ChatAdapterWorkflowMessenger {
    async fn send_targeted_activation(
        &self,
        channel: &ChannelRef,
        body: &str,
        target_user_id: u64,
    ) -> Result<Option<String>, MessengerError> {
        let target_user_id_text = target_user_id.to_string();
        let content = format!("{body}\n<@{target_user_id}>");
        let message = self
            .adapter
            .send_message_targeted(channel, &content, Some(&target_user_id_text))
            .await
            .map_err(|error| MessengerError::Transport(error.to_string()))?;
        if message.message_id.trim().is_empty() {
            return Err(MessengerError::NoMessageIdReturned);
        }
        Ok(Some(message.message_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::state::{WorkflowMode, WorkflowRole, WorkflowStage};
    use crate::workflow::WorkflowContext;
    use std::path::PathBuf;

    fn sample_context() -> WorkflowContext {
        WorkflowContext {
            workflow_id: "wf-2026-08-18".into(),
            project_id: "openab".into(),
            project_root: PathBuf::from("/home/arthur/openab/source"),
            assigned_role: WorkflowRole::Verifier,
            workflow_stage: WorkflowStage::VerifierActive,
            language: "zh-TW".into(),
            workflow_revision: 2,
            defect_loop_count: 0,
            transition_id: None,
            scope: None,
            unavailable_agents: Vec::new(),
            authorized_by: "Tech Lead".into(),
            thread_id: "1536735741642547262".into(),
            mode: WorkflowMode::ThreeAgent,
            last_committed_result: None,
        }
    }

    fn sample_context_for(role: WorkflowRole) -> WorkflowContext {
        let stage = match role {
            WorkflowRole::Primary => WorkflowStage::PrimaryCorrectionPending,
            WorkflowRole::Verifier => WorkflowStage::VerifierActive,
            WorkflowRole::FinalReviewer => WorkflowStage::FinalReviewerActive,
        };
        let mut ctx = sample_context();
        ctx.assigned_role = role;
        ctx.workflow_stage = stage;
        ctx
    }

    #[test]
    fn activation_body_contains_trusted_fields_only() {
        let ctx = sample_context();
        let body = render_activation_body(&ctx, "2c2bf05d5815f8b8a914fbbe02e23d4b");
        for needle in [
            "<workflow_activation>",
            "workflow_id: wf-2026-08-18",
            "project_id: openab",
            "project_root: /home/arthur/openab/source",
            "assigned_role: VERIFIER",
            "workflow_stage: VERIFIER_ACTIVE",
            "language: zh-TW",
            "workflow_revision: 2",
            "transition_id: 2c2bf05d5815f8b8a914fbbe02e23d4b",
            "</workflow_activation>",
        ] {
            assert!(body.contains(needle), "missing {needle:?}\n{body}");
        }
        // The body must NOT embed a HANDOFF *envelope* (the
        // terminal-line signature `HANDOFF COMPLETE — …`).
        // The constraint text inside `<role_completion_contract>`
        // legitimately tells the agent NOT to emit one; that
        // substring is expected and not a violation.
        assert!(
            !body.contains("HANDOFF COMPLETE"),
            "must not embed HANDOFF COMPLETE envelope\n{body}"
        );
        assert!(
            !body.contains("<@"),
            "must not embed Discord mention\n{body}"
        );
        // The contract's `forbidden_fields` block names
        // `next_role` / `next_stage` / `target_user_id` /
        // `transition_id` / `workflow_revision` so the agent
        // knows NOT to author those tokens. Test the contract
        // namespace explicitly instead of substring-match against
        // the whole body: those tokens must not appear as
        // agent-supplied `key: value` pairs.
        for forbidden_key in [
            "next_agent:",
            "next_role:",
            "next_stage:",
            "target_user_id:",
        ] {
            assert!(
                !body.contains(forbidden_key),
                "must not contain agent-supplied {forbidden_key:?} field\n{body}"
            );
        }
    }

    #[test]
    fn activation_body_round_trip_with_change_of_stage() {
        let mut ctx = sample_context();
        ctx.assigned_role = WorkflowRole::FinalReviewer;
        ctx.workflow_stage = WorkflowStage::FinalReviewerActive;
        let body = render_activation_body(&ctx, "abc123");
        assert!(body.contains("assigned_role: FINAL_REVIEWER"));
        assert!(body.contains("workflow_stage: FINAL_REVIEWER_ACTIVE"));
    }

    // ---------- Phase 4.2.1 role_completion_contract ----------

    /// A. VERIFIER downstream activation contains the canonical
    ///    `<role_completion_contract>` block with the exact
    ///    template, trusted fields pre-filled, and the role +
    ///    allowed_results = PASS | FAIL declarations.
    #[test]
    fn contract_is_present_in_verifier_activation() {
        let ctx = sample_context_for(WorkflowRole::Verifier);
        let body = render_activation_body(&ctx, "tx-1");
        assert!(
            body.contains("<role_completion_contract>"),
            "missing contract opening tag\n{body}"
        );
        assert!(
            body.contains("</role_completion_contract>"),
            "missing contract closing tag\n{body}"
        );
        // Contract attaches immediately after the activation.
        let act_close = body
            .find("</workflow_activation>")
            .expect("activation close");
        let contract_open = body
            .find("<role_completion_contract>")
            .expect("contract open");
        assert!(
            contract_open > act_close,
            "contract must be appended after </workflow_activation>\n{body}"
        );
        // B/A spec: role + allowed_results + forbidden + constraints.
        assert!(body.contains("role: VERIFIER\n"), "{body}");
        assert!(body.contains("allowed_results: PASS | FAIL\n"), "{body}");
        // Trusted field pre-fill.
        assert!(body.contains("workflow_id: wf-2026-08-18"), "{body}");
        assert!(body.contains("project_id: openab"), "{body}");
        assert!(
            body.contains("project_root: /home/arthur/openab/source"),
            "{body}"
        );
        // Constraint reminders.
        assert!(
            body.contains("do NOT output HANDOFF or any agent-routing envelope"),
            "{body}"
        );
        assert!(body.contains("OpenAB owns transition routing"), "{body}");
        assert!(body.contains("language: zh-TW\n"), "{body}");
    }

    /// B. FINAL_REVIEWER downstream activation contains the canonical
    ///    contract. Allowed results are PASS | FAIL.
    #[test]
    fn contract_is_present_in_final_reviewer_activation() {
        let ctx = sample_context_for(WorkflowRole::FinalReviewer);
        let body = render_activation_body(&ctx, "tx-2");
        assert!(body.contains("<role_completion_contract>"), "{body}");
        assert!(body.contains("role: FINAL_REVIEWER\n"), "{body}");
        assert!(body.contains("allowed_results: PASS | FAIL\n"), "{body}");
        assert!(body.contains("</role_completion_contract>"), "{body}");
    }

    /// C. Both PASS and FAIL are legal result tokens for the
    ///    VERIFIER / FINAL_REVIEWER contract.
    #[test]
    fn verifier_and_final_reviewer_contract_admits_pass_and_fail() {
        for role in [WorkflowRole::Verifier, WorkflowRole::FinalReviewer] {
            let ctx = sample_context_for(role);
            let body = render_role_completion_contract(&ctx);
            assert!(body.contains("allowed_results: PASS | FAIL"));
        }
    }

    /// PRIMARY (defect-loop reactivation) contract admits only
    /// `COMPLETE`. The PRIMARY contract uses the same canonical
    /// renderer — single source of truth.
    #[test]
    fn primary_contract_admits_only_complete() {
        let ctx = sample_context_for(WorkflowRole::Primary);
        let body = render_role_completion_contract(&ctx);
        assert!(body.contains("allowed_results: COMPLETE"));
        assert!(!body.contains("PASS"));
    }

    /// D. Contract binds trusted workflow_id / project_id /
    ///    project_root from the [`WorkflowContext`] snapshot —
    ///    never from agent-supplied input.
    #[test]
    fn contract_uses_trusted_workflow_identity() {
        let mut ctx = sample_context_for(WorkflowRole::Verifier);
        ctx.workflow_id = "wf-trusted-id-42".into();
        ctx.project_id = "trusted-project".into();
        ctx.project_root = PathBuf::from("/trusted/root/path");
        let body = render_activation_body(&ctx, "tx-3");
        assert!(body.contains("workflow_id: wf-trusted-id-42"));
        assert!(body.contains("project_id: trusted-project"));
        assert!(body.contains("project_root: /trusted/root/path"));
        // Output template re-uses the same trusted values.
        let contract = render_role_completion_contract(&ctx);
        assert!(contract.contains("workflow_id: wf-trusted-id-42"));
        assert!(contract.contains("project_id: trusted-project"));
        assert!(contract.contains("project_root: /trusted/root/path"));
    }

    /// E. Contract MUST NOT list any of the forbidden tokens as
    ///    agent-supplied fields. The forbidden_tokens may only
    ///    appear inside the `forbidden_fields` block of the
    ///    contract (i.e. preceded by `  - ` indentation). Anything
    ///    like `<forbidden>: <value>` would mean the renderer
    ///    gave the agent authority over routing/transition state,
    ///    which is forbidden.
    #[test]
    fn contract_does_not_supply_forbidden_fields() {
        for role in [
            WorkflowRole::Primary,
            WorkflowRole::Verifier,
            WorkflowRole::FinalReviewer,
        ] {
            let ctx = sample_context_for(role);
            let body = render_role_completion_contract(&ctx);
            for forbidden in [
                "transition_id:",
                "workflow_revision:",
                "next_role:",
                "next_stage:",
                "target_user_id:",
            ] {
                assert!(
                    !body.contains(forbidden),
                    "{role} contract must not supply agent-supplied {forbidden:?}\n{body}"
                );
            }
            // ...yet the contract MUST enumerate the same tokens
            // inside its `forbidden_fields:` list.
            assert!(body.contains("forbidden_fields:"));
            for listed in [
                "transition_id",
                "workflow_revision",
                "next_role",
                "next_stage",
                "target_user_id",
            ] {
                assert!(
                    body.contains(listed),
                    "{role} contract must enumerate {listed:?} inside forbidden_fields\n{body}"
                );
            }
        }
    }

    /// F. Activation must never produce a HANDOFF instruction
    ///    (plain-text `HANDOFF COMPLETE — awaiting Tech Lead
    ///    direction` and friends are forbidden). The contract
    ///    enforces this by explicitly forbidding the envelope.
    #[test]
    fn activation_body_does_not_emit_handoff_envelope() {
        for role in [
            WorkflowRole::Primary,
            WorkflowRole::Verifier,
            WorkflowRole::FinalReviewer,
        ] {
            let ctx = sample_context_for(role);
            let body = render_activation_body(&ctx, "tx-4");
            assert!(
                !body.contains("HANDOFF COMPLETE"),
                "{role} activation must not contain HANDOFF completion envelope\n{body}"
            );
            // The contract explicitly tells the agent not to
            // emit HANDOFF — that mention is fine in the
            // constraints block.
            assert!(
                body.contains("do NOT output HANDOFF"),
                "{role} activation must instruct the agent against HANDOFF\n{body}"
            );
        }
    }

    /// G. Language propagation: the contract must echo the
    ///    trusted `language` from the workflow context so the
    ///    downstream agent responds in the right language.
    #[test]
    fn contract_propagates_language() {
        for lang in ["zh-TW", "en", "ja", "zh-CN"] {
            let mut ctx = sample_context_for(WorkflowRole::Verifier);
            ctx.language = lang.into();
            let body = render_activation_body(&ctx, "tx-5");
            assert!(
                body.contains(&format!("language: {lang}\n")),
                "language {lang} must propagate into the activation body\n{body}"
            );
            let contract = render_role_completion_contract(&ctx);
            assert!(
                contract.contains(&format!("language: {lang}\n")),
                "language {lang} must propagate into the contract\n{contract}"
            );
        }
    }
}

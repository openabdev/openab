//! Phase 4.2 sandbox live fault-injection acceptance.
//!
//! Tests construct durable state directly in disposable sandbox
//! directories created via [`tempfile::TempDir`] (never the
//! production `.openab/` directory) and exercise the public
//! recovery primitives end-to-end. The hermetic guarantee is
//! twofold:
//!
//! - Every test gets a unique sandbox root (per-process, per-OS-tmp),
//!   so the suite is safe to run back-to-back without manual cleanup.
//! - The [`tempfile::TempDir`] guard is held alive for the entire
//!   test, so the directory persists until the test completes; once
//!   the guard is dropped at the end of the test, the directory is
//!   removed by `tempfile`'s destructor.
//!
//! Acceptance scenarios covered:
//!
//! Three acceptance scenarios are covered:
//!
//! - **A**: Stale `RESERVED`. Verify recovery releases the row
//!   and a subsequent identical completion reaches DELIVERED
//!   with exactly one targeted send.
//! - **B**: Assignment advanced / ledger `PENDING`. Verify
//!   recovery reconciles the ledger using the durable
//!   `openab_message_id` recorded in the assignment; no resend,
//!   no revision increment. Two sub-variants: targeted (mid set)
//!   and terminal (`mid = None`).
//! - **C**: `UNKNOWN_DELIVERY`. Verify recovery leaves state
//!   untouched, surfaces an operator-visible `UNKNOWN_DELIVERY`,
//!   performs zero resends.
//!
//! These tests bypass `cfg(test)` failpoints entirely. Pre-state
//! is constructed by writing JSON directly to
//! `<project_root>/.openab/workflow_assignment.json` and
//! `<project_root>/.openab/workflow_transitions.json`. The
//! recovery primitives operate only on those files; no test-only
//! instrumentation runs.
//!
//! No production daemon is started, no production `.openab/`
//! directory is touched, no real Discord network call is made.
//! The recorder messenger captures sends without any Discord IO.
//!
//! # Hermeticity
//!
//! Back-to-back invocations of this binary are independent:
//! there is no shared mutable state between two test runs. Each
//! `#[test]` instantiates its own `tempfile::TempDir` via
//! [`sandbox_root`] and the directory is removed when the guard
//! is dropped at the end of the test body. No `rm -rf` is
//! required between runs; the OS tmp directory is the only
//! shared resource, and `tempfile` namespaces it by process id
//! plus a per-test label.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use openab_core::adapter::ChannelRef;
use openab_core::workflow::assignment::{
    load_assignment, save_assignment_atomic, WorkflowAssignment,
};
use openab_core::workflow::handoff::{MessengerError, WorkflowMessenger};
use openab_core::workflow::ledger::{TransitionLedger, TransitionStatus};
use openab_core::workflow::recovery::{reconcile_project_workflow, RecoveryOutcome};
use openab_core::workflow::state::{
    legal_next_stage, CompletionResult, WorkflowRole, WorkflowStage,
};
use openab_core::workflow::transition_id::derive_transition_id;
use openab_core::workflow::WorkflowService;

// ----- Constants -----------------------------------------------------

const WORKFLOW_ID: &str = "wf-2026-08-18-acceptance-A-B-C";
const PROJECT_ID: &str = "openab-phase-4.2-sandbox";
const THREAD_ID: &str = "1539431300317061231-sandbox";
const WF_REV: u64 = 0;

const CLAUDE_UID: u64 = 1536733602304499852;
const CODEX_UID: u64 = 1536734779607879700;
const GEMINI_UID: u64 = 1536737891231866971;

// Wire-format names matching the on-disk serde renames.
const STAGE_PRIMARY_ACTIVE: &str = "PRIMARY_ACTIVE";
const STAGE_FINAL_REVIEWER_ACTIVE: &str = "FINAL_REVIEWER_ACTIVE";
const ROLE_PRIMARY: &str = "PRIMARY";
const ROLE_FINAL_REVIEWER: &str = "FINAL_REVIEWER";
const RESULT_COMPLETE: &str = "COMPLETE";
const RESULT_PASS: &str = "PASS";
const STATUS_RESERVED: &str = "Reserved";
const STATUS_PENDING: &str = "Pending";

// ----- Helpers ------------------------------------------------------

/// Disposable, hermetic sandbox root for a single acceptance
/// scenario. The returned `tempfile::TempDir` MUST be kept alive
/// for the duration of the test — dropping it removes the
/// underlying directory. Callers typically bind the guard to
/// `let _sandbox = ...;` and use the `PathBuf` for all
/// file-system operations inside the test body.
///
/// Each call returns a fresh, unique OS-level tmp directory; two
/// back-to-back invocations of this function never share any
/// on-disk state. No `rm -rf` is required between runs.
fn sandbox_root(scenario: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("openab-phase-4.2-sandbox-{scenario}-"))
        .suffix("-acceptance")
        .tempdir()
        .expect("create hermetic sandbox tempdir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

fn openab_dir(root: &Path) -> PathBuf {
    root.join(".openab")
}

fn write_initial_json(root: &Path) {
    let dir = openab_dir(root);
    fs::create_dir_all(&dir).unwrap();
}

fn fresh_assignment(state: WorkflowStage, revision: u64, root: &Path) -> WorkflowAssignment {
    WorkflowAssignment {
        schema_version: "v2".into(),
        workflow_id: WORKFLOW_ID.into(),
        project_id: PROJECT_ID.into(),
        project_root: root.to_path_buf(),
        mode: Default::default(),
        primary: "ArthurClaude".into(),
        verifier: "ArthurCodex".into(),
        final_reviewer: "ArthurGemini".into(),
        state,
        workflow_revision: revision,
        defect_loop_count: 0,
        language: "zh-TW".into(),
        thread_id: THREAD_ID.into(),
        last_transition_id: None,
        last_delivery_message_id: None,
        unavailable_agents: Vec::new(),
        authorized_by: "Tech Lead".into(),
        reason: "phase-4.2 sandbox acceptance".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn write_assignment(root: &Path, a: &WorkflowAssignment) {
    save_assignment_atomic(root, a).expect("assignment save");
}

fn bot_user_ids() -> std::collections::HashMap<String, u64> {
    let mut m = std::collections::HashMap::new();
    m.insert("ArthurClaude".into(), CLAUDE_UID);
    m.insert("ArthurCodex".into(), CODEX_UID);
    m.insert("ArthurGemini".into(), GEMINI_UID);
    m
}

fn primary_complete_id() -> String {
    derive_transition_id(
        WORKFLOW_ID,
        WF_REV,
        WorkflowStage::PrimaryActive,
        WorkflowRole::Primary,
        CompletionResult::Complete,
    )
}

fn reserve_primary_complete_id_in(root: &Path) -> String {
    let mut l = TransitionLedger::load(root).unwrap();
    assert!(l
        .reserve(
            WORKFLOW_ID,
            WF_REV,
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        )
        .is_ok());
    l.save_atomic().unwrap();
    primary_complete_id()
}

/// Build the JSON representation of a ledger row using
/// wire-format strings. Re-uses this everywhere so the
/// on-disk serde renames cannot accidentally drift from
/// the test fixtures.
#[allow(clippy::too_many_arguments)]
fn ledger_entry_json(
    transition_id: &str,
    wf_revision: u64,
    stage: &str,
    role: &str,
    result: &str,
    status: &str,
    target_user_id: Option<&str>,
    openab_message_id: Option<&str>,
) -> Value {
    json!({
        "transition_id": transition_id,
        "workflow_id": WORKFLOW_ID,
        "workflow_revision": wf_revision,
        "current_stage": stage,
        "role": role,
        "result": result,
        "status": status,
        "openab_message_id": openab_message_id,
        "target_user_id": target_user_id,
        "created_at": "2026-08-19T00:00:00Z",
        "delivered_at": null,
    })
}

fn write_ledger_json(root: &Path, entries: &[Value]) {
    fs::write(
        openab_dir(root).join("workflow_transitions.json"),
        serde_json::to_string_pretty(&Value::Array(entries.to_vec())).unwrap(),
    )
    .unwrap();
}

fn read_ledger_json(root: &Path) -> Value {
    let raw = fs::read_to_string(openab_dir(root).join("workflow_transitions.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn read_assignment_json(root: &Path) -> Value {
    let raw = fs::read_to_string(openab_dir(root).join("workflow_assignment.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn status_of_field(value: &Value, status: &str) -> bool {
    value.get("status").and_then(|v| v.as_str()) == Some(status)
}

// ----- A recorder messenger that records every send -------------------

#[derive(Default)]
struct RecordingMessenger {
    sent: Mutex<Vec<(String, u64)>>,
}

#[async_trait::async_trait]
impl WorkflowMessenger for RecordingMessenger {
    async fn send_targeted_activation(
        &self,
        _channel: &ChannelRef,
        body: &str,
        target_user_id: u64,
    ) -> Result<Option<String>, MessengerError> {
        self.sent
            .lock()
            .unwrap()
            .push((body.to_string(), target_user_id));
        Ok(Some(format!(
            "[SANDBOX-PHASE-4.2] discord-msg-{}",
            self.sent.lock().unwrap().len()
        )))
    }
}

fn recording_messenger() -> Arc<RecordingMessenger> {
    Arc::new(RecordingMessenger::default())
}

// ----- AC-A — Stale RESERVED release --------------------------------

#[test]
fn acceptance_a_stale_reserved_releases_and_retry_succeeds() {
    let (_sandbox, root) = sandbox_root("a");
    write_initial_json(&root);

    let pre_assignment = fresh_assignment(WorkflowStage::PrimaryActive, WF_REV, &root);
    write_assignment(&root, &pre_assignment);
    let id = reserve_primary_complete_id_in(&root);

    // Pre-state: ledger has a single RESERVED row for our
    // transition_id. No mid, no target_user_id, no assignment
    // advance.
    write_ledger_json(
        &root,
        &[ledger_entry_json(
            &id,
            WF_REV,
            STAGE_PRIMARY_ACTIVE,
            ROLE_PRIMARY,
            RESULT_COMPLETE,
            STATUS_RESERVED,
            None,
            None,
        )],
    );

    // Sanity.
    {
        let l = TransitionLedger::load(&root).unwrap();
        assert_eq!(l.entries().len(), 1);
        assert!(matches!(l.status_of(&id), Some(TransitionStatus::Reserved)));
    }

    // (1) Recovery invocation.
    let report = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report.persisted);
    assert_eq!(report.released_reserved.len(), 1);
    assert!(report.released_reserved.contains(&id));
    assert!(report.unknown_delivery.is_empty());
    assert!(report.reconciled_delivered.is_empty());

    // (2) Post-state: row gone, assignment unchanged.
    let post_l = TransitionLedger::load(&root).unwrap();
    assert!(post_l.entries().is_empty());
    let post_a = load_assignment(&root).unwrap().unwrap();
    assert_eq!(post_a.workflow_revision, WF_REV);
    assert_eq!(post_a.state, WorkflowStage::PrimaryActive);
    assert!(post_a.last_transition_id.is_none());

    // (3) Re-record the recovery evidence to the audit dump.
    eprintln!(
        "[AUDIT/A] recovery: released_reserved={:?}; unknown_delivery={:?}; reconciled={:?}; persisted={}",
        report.released_reserved, report.unknown_delivery, report.reconciled_delivered, report.persisted
    );

    // (4) Subsequent identical completion: a fresh WorkflowService
    //     invokes commit_protocol exactly once through the happy path.
    let messenger = recording_messenger();
    let svc = WorkflowService::new(
        std::collections::HashSet::new(),
        bot_user_ids(),
        messenger.clone() as Arc<dyn WorkflowMessenger>,
    );
    let body = format!(
        "<role_completion>\nrole: PRIMARY\nresult: COMPLETE\n\
         workflow_id: {WORKFLOW_ID}\nproject_id: {PROJECT_ID}\n\
         project_root: {}\n</role_completion>",
        root.display()
    );
    let channel = ChannelRef {
        platform: "discord".into(),
        channel_id: THREAD_ID.into(),
        thread_id: None,
        parent_id: None,
        origin_event_id: None,
    };
    let outcome = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(svc.on_turn_complete(
            &format!("discord:{THREAD_ID}"),
            Some(&root),
            &channel,
            &body,
            true,
        ))
    };

    match outcome {
        openab_core::workflow::TurnOutcome::Accepted {
            next_stage,
            transition_id: vid,
            ..
        } => {
            assert_eq!(next_stage, WorkflowStage::VerifierActive);
            assert_eq!(vid, id);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }

    let sent = messenger.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "exactly one targeted send after retry");
    assert_eq!(sent[0].1, CODEX_UID, "Codex is the recipient");
    // The body is rendered by `render_activation_body`; we do not
    // assert on its exact contents (they are part of the daemon's
    // own formatting). What matters here is the count, the target,
    // and the resulting assignment + ledger state.

    let final_l = TransitionLedger::load(&root).unwrap();
    assert_eq!(final_l.entries().len(), 1);
    assert!(matches!(
        final_l.status_of(&id),
        Some(TransitionStatus::Delivered)
    ));
    let entry = final_l.lookup(&id).unwrap();
    assert_eq!(
        entry.target_user_id.as_deref(),
        Some(CODEX_UID.to_string().as_str())
    );
    assert!(entry.openab_message_id.is_some());

    let final_a = load_assignment(&root).unwrap().unwrap();
    assert_eq!(final_a.workflow_revision, 1);
    assert_eq!(final_a.state, WorkflowStage::VerifierActive);
    assert_eq!(final_a.last_transition_id.as_deref(), Some(id.as_str()));

    eprintln!(
        "[AUDIT/A] retry through happy path: sent={:?} mid={:?} final_revision={}",
        sent.iter().map(|(_, u)| *u).collect::<Vec<_>>(),
        entry.openab_message_id,
        final_a.workflow_revision,
    );
}

// ----- AC-B (targeted) — Assignment advanced / ledger PENDING ----

#[test]
fn acceptance_b_assignment_advanced_reconciles_to_delivered() {
    let (_sandbox, root) = sandbox_root("b");
    write_initial_json(&root);

    let mid = "[SANDBOX-PHASE-4.2] B-mid-9001".to_string();
    let id = derive_transition_id(
        WORKFLOW_ID,
        WF_REV,
        WorkflowStage::PrimaryActive,
        WorkflowRole::Primary,
        CompletionResult::Complete,
    );

    // Pre-state: assignment advanced to next_stage+1.
    let mut pre_assignment = fresh_assignment(WorkflowStage::PrimaryActive, WF_REV, &root);
    pre_assignment.workflow_revision = WF_REV + 1;
    pre_assignment.state = WorkflowStage::VerifierActive;
    pre_assignment.last_transition_id = Some(id.clone());
    pre_assignment.last_delivery_message_id = Some(mid.clone());
    write_assignment(&root, &pre_assignment);

    write_ledger_json(
        &root,
        &[ledger_entry_json(
            &id,
            WF_REV,
            STAGE_PRIMARY_ACTIVE,
            ROLE_PRIMARY,
            RESULT_COMPLETE,
            STATUS_PENDING,
            Some(&CODEX_UID.to_string()),
            None,
        )],
    );

    // (1) Recovery.
    let report = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report.persisted);
    assert!(report.unknown_delivery.is_empty());
    assert_eq!(report.reconciled_delivered.len(), 1);
    assert!(report.reconciled_delivered.contains(&id));

    // (2) Ledger becomes DELIVERED with the durable mid.
    let l = TransitionLedger::load(&root).unwrap();
    let entry = l.lookup(&id).unwrap();
    assert_eq!(entry.status, TransitionStatus::Delivered);
    assert_eq!(entry.openab_message_id.as_deref(), Some(mid.as_str()));

    // (3) Assignment is NOT rewritten.
    let a = load_assignment(&root).unwrap().unwrap();
    assert_eq!(a.workflow_revision, WF_REV + 1);
    assert_eq!(a.state, WorkflowStage::VerifierActive);

    // (4) Second recovery pass is a no-op.
    let report2 = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report2.persisted);
    assert!(report2.reconciled_delivered.is_empty());
    assert!(report2.unknown_delivery.is_empty());
    assert_eq!(report2.delivered_noop, 1);

    // (5) Snapshot files.
    eprintln!(
        "[AUDIT/B] post-recovery assignment={}",
        read_assignment_json(&root)
    );
    eprintln!("[AUDIT/B] post-recovery ledger={}", read_ledger_json(&root));

    // (6) legal_next_stage sanity.
    assert_eq!(
        legal_next_stage(
            WorkflowStage::PrimaryActive,
            WorkflowRole::Primary,
            CompletionResult::Complete,
        ),
        Some(WorkflowStage::VerifierActive)
    );
}

// ----- AC-B (terminal variant) -------------------------------------

#[test]
fn acceptance_b_terminal_reconciles_with_null_mid() {
    let (_sandbox, root) = sandbox_root("b_terminal");
    write_initial_json(&root);

    let id = derive_transition_id(
        WORKFLOW_ID,
        WF_REV,
        WorkflowStage::FinalReviewerActive,
        WorkflowRole::FinalReviewer,
        CompletionResult::Pass,
    );

    let mut pre_assignment = fresh_assignment(WorkflowStage::FinalReviewerActive, WF_REV, &root);
    pre_assignment.workflow_revision = WF_REV + 1;
    pre_assignment.state = WorkflowStage::TechLeadWait;
    pre_assignment.last_transition_id = Some(id.clone());
    pre_assignment.last_delivery_message_id = None;
    write_assignment(&root, &pre_assignment);

    write_ledger_json(
        &root,
        &[ledger_entry_json(
            &id,
            WF_REV,
            STAGE_FINAL_REVIEWER_ACTIVE,
            ROLE_FINAL_REVIEWER,
            RESULT_PASS,
            STATUS_PENDING,
            None,
            None,
        )],
    );

    let report = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report.persisted);
    assert_eq!(report.reconciled_delivered.len(), 1);

    let l = TransitionLedger::load(&root).unwrap();
    let entry = l.lookup(&id).unwrap();
    assert_eq!(entry.status, TransitionStatus::Delivered);
    assert!(
        entry.openab_message_id.is_none(),
        "terminal transition permits null message_id"
    );
    assert!(entry.target_user_id.is_none());

    let a = load_assignment(&root).unwrap().unwrap();
    assert_eq!(a.workflow_revision, WF_REV + 1);
    assert_eq!(a.state, WorkflowStage::TechLeadWait);
}

// ----- AC-C — UNKNOWN_DELIVERY --------------------------------------

#[test]
fn acceptance_c_unknown_delivery_no_resend_no_commit() {
    let (_sandbox, root) = sandbox_root("c");
    write_initial_json(&root);

    let id = derive_transition_id(
        WORKFLOW_ID,
        WF_REV,
        WorkflowStage::PrimaryActive,
        WorkflowRole::Primary,
        CompletionResult::Complete,
    );

    // Pre-state: assignment OLD (never advanced).
    let pre_assignment = fresh_assignment(WorkflowStage::PrimaryActive, WF_REV, &root);
    write_assignment(&root, &pre_assignment);

    write_ledger_json(
        &root,
        &[ledger_entry_json(
            &id,
            WF_REV,
            STAGE_PRIMARY_ACTIVE,
            ROLE_PRIMARY,
            RESULT_COMPLETE,
            STATUS_PENDING,
            Some(&CODEX_UID.to_string()),
            None,
        )],
    );

    // (1) First recovery surfaces UNKNOWN_DELIVERY.
    let report = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report.persisted);
    assert_eq!(report.unknown_delivery.len(), 1);
    assert!(report.unknown_delivery.contains(&id));
    assert!(report.released_reserved.is_empty());
    assert!(report.reconciled_delivered.is_empty());
    let saw_unk = report
        .outcomes
        .iter()
        .any(|o| matches!(o, RecoveryOutcome::UnknownDelivery(t) if t == &id));
    assert!(saw_unk);

    // (2) State unchanged.
    let l = TransitionLedger::load(&root).unwrap();
    let entry = l.lookup(&id).unwrap();
    assert_eq!(entry.status, TransitionStatus::Pending);
    assert!(
        entry.openab_message_id.is_none(),
        "recovery must not fabricate a message_id"
    );
    assert_eq!(
        entry.target_user_id.as_deref(),
        Some(CODEX_UID.to_string().as_str())
    );

    let a = load_assignment(&root).unwrap().unwrap();
    assert_eq!(a.workflow_revision, WF_REV);
    assert_eq!(a.state, WorkflowStage::PrimaryActive);
    assert!(a.last_transition_id.is_none());

    // (3) Repeated recovery remains fail-closed.
    let report2 = reconcile_project_workflow(&root).expect("recovery ok");
    assert!(report2.persisted);
    assert_eq!(report2.unknown_delivery.len(), 1);

    // (4) Ledger still exactly one row, PENDING, no mid.
    let final_json = read_ledger_json(&root);
    let arr = final_json.as_array().expect("ledger is array");
    assert_eq!(arr.len(), 1);
    let only = &arr[0];
    assert_eq!(
        only.get("transition_id").and_then(|v| v.as_str()),
        Some(id.as_str())
    );
    assert!(status_of_field(only, STATUS_PENDING));
    assert!(
        only.get("openab_message_id").is_none() || only.get("openab_message_id").unwrap().is_null()
    );
}

// ----- Cross-acceptance audit dump -----------------------------

#[test]
fn acceptance_audit_dump_before_after() {
    // Each iteration owns its own TempDir guard so the directory
    // persists until the iteration's body has finished reading.
    let scenarios = [("A", "a"), ("B", "b"), ("B-term", "b_terminal"), ("C", "c")];
    let mut owned: Vec<(tempfile::TempDir, &str, PathBuf)> = Vec::new();
    for (label, scenario) in scenarios.iter() {
        let (dir, root) = sandbox_root(scenario);
        owned.push((dir, label, root));
    }
    for (_dir, label, root) in owned.iter() {
        let assn_path = openab_dir(root).join("workflow_assignment.json");
        let ledg_path = openab_dir(root).join("workflow_transitions.json");
        eprintln!("[AUDIT/{label}] project_root={}", root.display());
        if assn_path.exists() {
            eprintln!(
                "[AUDIT/{label}] workflow_assignment.json={}",
                fs::read_to_string(&assn_path)
                    .ok()
                    .unwrap_or_default()
                    .replace('\n', "\\n")
            );
        }
        if ledg_path.exists() {
            eprintln!(
                "[AUDIT/{label}] workflow_transitions.json={}",
                fs::read_to_string(&ledg_path)
                    .ok()
                    .unwrap_or_default()
                    .replace('\n', "\\n")
            );
        }
    }
}

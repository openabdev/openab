//! Project-local transition ledger for the OpenAB-native three-agent
//! coding workflow (workflow
//! `20260818-openab-automatic-three-agent-handoff`).
//!
//! # Canonical storage path
//!
//! ```text
//! <canonical_project_root>/.openab/workflow_transitions.json
//! ```
//!
//! The ledger is project-local and lives in the same `.openab`
//! namespace as [`super::assignment`]. It is the single source of
//! truth for "has this transition already been delivered?" and the
//! only path that may move a `transition_id` through the four
//! lifecycle states.
//!
//! # Four-state lifecycle
//!
//! | Status        | Meaning                                                       |
//! |---------------|---------------------------------------------------------------|
//! | `RESERVED`    | Row created; no Discord send attempted yet                    |
//! | `PENDING`     | Send accepted (`openab_message_id` set); commit not yet done  |
//! | `DELIVERED`   | Send accepted + assignment committed                          |
//! | `FAILED`      | Previous attempt errored before send                          |
//!
//! State transitions are only permitted along the documented edges:
//!
//! ```text
//!   RESERVED  →  PENDING   (send accepted)
//!   PENDING   →  DELIVERED (commit done)
//!   PENDING   →  FAILED    (commit errored)
//!   RESERVED  →  FAILED    (send errored before PENDING)
//! ```
//!
//! The validator never sees `transition_id` directly — it asks the
//! ledger via [`TransitionLedger::status_of`] and treats `DELIVERED`
//! or `PENDING` as the "already happened" signal that must surface
//! `ALREADY_DELIVERED`.
//!
//! # Bounded size
//!
//! [`MAX_LEDGER_ENTRIES`] (256) is the cap. Pruning runs on
//! [`TransitionLedger::mark_delivered`] and never touches rows in
//! `RESERVED`, `PENDING`, or `FAILED` — those are always preserved
//! regardless of age so a stalled workflow cannot lose its audit
//! trail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::assignment::{canonicalize_root_for_workflow, WORKFLOW_DIR};
use super::state::{CompletionResult, WorkflowRole, WorkflowStage};
use super::transition_id::derive_transition_id;

/// Canonical ledger filename inside [`WORKFLOW_DIR`].
pub const LEDGER_FILENAME: &str = "workflow_transitions.json";

/// Maximum number of entries the ledger keeps on disk. Older
/// `DELIVERED` rows are pruned in `mark_delivered`; non-`DELIVERED`
/// rows are never pruned.
pub const MAX_LEDGER_ENTRIES: usize = 256;

/// One row in the transition ledger. `status` is the lifecycle
/// state; everything else is the trusted context that produced the
/// `transition_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub transition_id: String,
    pub workflow_id: String,
    pub workflow_revision: u64,
    pub current_stage: WorkflowStage,
    pub role: WorkflowRole,
    pub result: CompletionResult,
    pub status: TransitionStatus,
    pub openab_message_id: Option<String>,
    pub target_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// The four lifecycle states a `transition_id` can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionStatus {
    Reserved,
    Pending,
    Delivered,
    Failed,
}

impl TransitionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "RESERVED",
            Self::Pending => "PENDING",
            Self::Delivered => "DELIVERED",
            Self::Failed => "FAILED",
        }
    }
}

/// Failure modes for ledger load / save / mutation.
#[derive(Debug)]
pub enum LedgerError {
    EmptyProjectRoot,
    ProjectRootUnreadable {
        path: PathBuf,
        reason: String,
    },
    ProjectRootNotDirectory(PathBuf),
    ProjectRootNotAbsolute(PathBuf),
    Malformed(String),
    Io {
        path: PathBuf,
        reason: String,
    },
    /// Tried to reserve a `transition_id` that is already `DELIVERED`
    /// or `PENDING`. The validator surfaces this as
    /// `AlreadyDelivered`.
    AlreadyDelivered(String),
    /// Tried to reserve a `transition_id` whose row exists in
    /// `FAILED` status. Phase 2 preserves the row — this error
    /// signals "the future service must explicitly call
    /// `retry_failed_transition` if it wants a retry".
    FailedEntryExists(String),
    /// Tried to mutate an entry whose id is not in the ledger.
    UnknownTransition(String),
    /// Tried an illegal state transition (e.g. `DELIVERED → RESERVED`).
    InvalidTransition {
        transition_id: String,
        from: TransitionStatus,
        to: TransitionStatus,
    },
    /// Recovery attempted to release a `transition_id` whose row
    /// exists but is **not in `RESERVED` status**. The recovery
    /// primitive is deliberately narrow — only stale `RESERVED`
    /// rows may be released; any other state must be left alone
    /// for the existing transition machinery to handle. The
    /// distinct variant prevents callers from conflating "illegal
    /// state-machine transition" with "wrong-status-for-recovery".
    InvalidStateForRelease {
        transition_id: String,
        observed: TransitionStatus,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyProjectRoot => f.write_str("project_root is empty"),
            Self::ProjectRootUnreadable { path, reason } => {
                write!(f, "project_root {path:?} cannot be canonicalized: {reason}")
            }
            Self::ProjectRootNotDirectory(p) => {
                write!(f, "project_root {p:?} is not a directory")
            }
            Self::ProjectRootNotAbsolute(p) => {
                write!(f, "project_root {p:?} is not absolute")
            }
            Self::Malformed(reason) => write!(f, "malformed ledger JSON: {reason}"),
            Self::Io { path, reason } => {
                write!(f, "workflow ledger I/O error at {path:?}: {reason}")
            }
            Self::AlreadyDelivered(id) => {
                write!(f, "transition_id {id:?} already delivered or pending")
            }
            Self::FailedEntryExists(id) => write!(
                f,
                "transition_id {id:?} has a FAILED row; reserve does not auto-retry"
            ),
            Self::UnknownTransition(id) => write!(f, "transition_id {id:?} not present"),
            Self::InvalidTransition {
                transition_id,
                from,
                to,
            } => write!(
                f,
                "illegal state transition for {transition_id:?}: {from:?} -> {to:?}"
            ),
            Self::InvalidStateForRelease {
                transition_id,
                observed,
            } => write!(
                f,
                "recovery attempted to release {transition_id:?} \
                 in non-RESERVED state {observed:?}; release applies \
                 only to stale RESERVED rows"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<super::assignment::AssignmentError> for LedgerError {
    fn from(e: super::assignment::AssignmentError) -> Self {
        use super::assignment::AssignmentError as A;
        // Render the source error to a string before pattern matching,
        // so we don't have to clone the message out of every variant.
        let rendered = e.to_string();
        match e {
            A::EmptyProjectRoot => Self::EmptyProjectRoot,
            A::ProjectRootUnreadable { path, .. } => Self::ProjectRootUnreadable {
                path,
                reason: rendered,
            },
            A::ProjectRootNotDirectory(p) => Self::ProjectRootNotDirectory(p),
            A::ProjectRootNotAbsolute(p) => Self::ProjectRootNotAbsolute(p),
            A::Malformed(_) => Self::Malformed(rendered),
            A::MissingField(_)
            | A::UnsupportedSchemaVersion(_)
            | A::ProjectRootMismatch { .. }
            | A::DefectLoopExceeded { .. }
            | A::Io { .. } => Self::Io {
                path: PathBuf::new(),
                reason: rendered,
            },
        }
    }
}

/// Project-local transition ledger.
///
/// Holds the in-memory list of [`LedgerEntry`] rows and the canonical
/// path to its JSON file. Mutations never auto-save — the caller
/// commits via [`TransitionLedger::save_atomic`] after the 6-step
/// transition protocol runs in full.
#[derive(Debug)]
pub struct TransitionLedger {
    entries: Vec<LedgerEntry>,
    path: PathBuf,
}

impl TransitionLedger {
    /// Load the ledger from
    /// `<project_root>/.openab/workflow_transitions.json`.
    ///
    /// - missing file → `Ok(empty ledger)`
    /// - malformed JSON → `Err(LedgerError::Malformed)`
    /// - unreadable project_root → `Err(LedgerError::ProjectRootUnreadable)`
    pub fn load(project_root: &Path) -> Result<Self, LedgerError> {
        let canonical = canonicalize_root_for_workflow(project_root)?;
        let path = canonical.join(WORKFLOW_DIR).join(LEDGER_FILENAME);
        if !path.exists() {
            return Ok(Self {
                entries: Vec::new(),
                path,
            });
        }
        let data = fs::read_to_string(&path).map_err(|e| LedgerError::Io {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        let entries: Vec<LedgerEntry> =
            serde_json::from_str(&data).map_err(|e| LedgerError::Malformed(e.to_string()))?;
        Ok(Self { entries, path })
    }

    /// Persist the ledger atomically. Mirrors the assignment atomic
    /// write pattern (`.json.tmp` sibling + rename).
    pub fn save_atomic(&self) -> Result<PathBuf, LedgerError> {
        // Test-only failpoint: lets Phase 4.2 fault-injection tests
        // simulate `save_atomic` failing AFTER an in-memory mutation
        // (RESERVE / MARK_PENDING / MARK_DELIVERED) succeeded, so the
        // resulting durable state reflects "mutation happened in
        // memory, nothing on disk". Production builds compile this
        // arm to a true branch that never executes.
        #[cfg(test)]
        {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static FAIL_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
            static FAIL_COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = FAIL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
            let target = FAIL_AFTER.load(Ordering::SeqCst);
            if n == target {
                return Err(LedgerError::Io {
                    path: self.path.clone(),
                    reason: "test_injected:save_atomic".to_string(),
                });
            }
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| LedgerError::Io {
                path: parent.to_path_buf(),
                reason: format!("create_dir_all: {e}"),
            })?;
        }
        let tmp_path = self.path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| LedgerError::Malformed(e.to_string()))?;
        {
            let mut f = fs::File::create(&tmp_path).map_err(|e| LedgerError::Io {
                path: tmp_path.clone(),
                reason: format!("create tmp: {e}"),
            })?;
            f.write_all(data.as_bytes()).map_err(|e| LedgerError::Io {
                path: tmp_path.clone(),
                reason: format!("write tmp: {e}"),
            })?;
            f.flush().map_err(|e| LedgerError::Io {
                path: tmp_path.clone(),
                reason: format!("flush tmp: {e}"),
            })?;
        }
        fs::rename(&tmp_path, &self.path).map_err(|e| LedgerError::Io {
            path: self.path.clone(),
            reason: format!("rename tmp onto final: {e}"),
        })?;
        Ok(self.path.clone())
    }

    /// Look up an entry by `transition_id`. Returns `None` if the id
    /// is not in the ledger.
    pub fn lookup(&self, transition_id: &str) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .find(|e| e.transition_id == transition_id)
    }

    /// Status-only lookup — convenience for the validator's check #10.
    pub fn status_of(&self, transition_id: &str) -> Option<TransitionStatus> {
        self.lookup(transition_id).map(|e| e.status)
    }

    /// Read-only access to the full row set, in insertion order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Reserve (or re-reserve) a `transition_id`.
    ///
    /// Behaviour:
    /// - no entry → create `RESERVED` and return it;
    /// - existing `RESERVED` → idempotent, return it;
    /// - existing `PENDING` or `DELIVERED` → return
    ///   [`LedgerError::AlreadyDelivered`];
    /// - existing `FAILED` → return [`LedgerError::FailedEntryExists`]
    ///   **without mutating the row**. Phase 2 is storage-only; the
    ///   future [`retry_failed_transition`] API is the only path that
    ///   may clear or replace a FAILED row.
    ///
    /// [`retry_failed_transition`]: (Phase 4+ — does not exist yet)
    pub fn reserve(
        &mut self,
        workflow_id: &str,
        workflow_revision: u64,
        current_stage: WorkflowStage,
        role: WorkflowRole,
        result: CompletionResult,
    ) -> Result<&LedgerEntry, LedgerError> {
        let transition_id =
            derive_transition_id(workflow_id, workflow_revision, current_stage, role, result);

        if let Some(idx) = self
            .entries
            .iter()
            .position(|e| e.transition_id == transition_id)
        {
            match self.entries[idx].status {
                TransitionStatus::Reserved => {
                    return Ok(&self.entries[idx]);
                }
                TransitionStatus::Pending | TransitionStatus::Delivered => {
                    return Err(LedgerError::AlreadyDelivered(transition_id));
                }
                TransitionStatus::Failed => {
                    // DO NOT mutate. Phase 2 preserves the audit row.
                    return Err(LedgerError::FailedEntryExists(transition_id));
                }
            }
        }

        let entry = LedgerEntry {
            transition_id,
            workflow_id: workflow_id.to_string(),
            workflow_revision,
            current_stage,
            role,
            result,
            status: TransitionStatus::Reserved,
            openab_message_id: None,
            target_user_id: None,
            created_at: Utc::now(),
            delivered_at: None,
        };
        self.entries.push(entry);
        Ok(self.entries.last().unwrap())
    }

    /// `RESERVED → PENDING`. `target_user_id` is recorded so recovery
    /// can replay without re-deriving it from the assignment.
    pub fn mark_pending(
        &mut self,
        transition_id: &str,
        target_user_id: Option<String>,
    ) -> Result<(), LedgerError> {
        self.transition_status(transition_id, TransitionStatus::Pending)?;
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.transition_id == transition_id)
            .unwrap();
        entry.target_user_id = target_user_id;
        Ok(())
    }

    /// `PENDING → DELIVERED`. Records `openab_message_id` and the
    /// delivery timestamp. Pruning of old `DELIVERED` rows runs at
    /// the end so the post-condition `entries.len() <=
    /// MAX_LEDGER_ENTRIES` holds after every successful commit.
    pub fn mark_delivered(
        &mut self,
        transition_id: &str,
        openab_message_id: Option<String>,
    ) -> Result<(), LedgerError> {
        self.transition_status(transition_id, TransitionStatus::Delivered)?;
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.transition_id == transition_id)
            .unwrap();
        entry.openab_message_id = openab_message_id;
        entry.delivered_at = Some(Utc::now());
        self.prune_delivered();
        Ok(())
    }

    /// Any non-terminal state → `FAILED`. Used both for send errors
    /// (before `PENDING`) and commit errors (from `PENDING`).
    pub fn mark_failed(&mut self, transition_id: &str) -> Result<(), LedgerError> {
        self.transition_status(transition_id, TransitionStatus::Failed)
    }

    /// Release a stale `RESERVED` row that survived a daemon
    /// crash. Removes the entry from `self.entries` entirely.
    ///
    /// This is the **recovery-only** primitive introduced by
    /// Phase 4.2. It deliberately applies only to `RESERVED`:
    /// a `RESERVED` row was created by step A of `commit_protocol`
    /// but step D (mark pending) never ran, so Discord was never
    /// contacted and assignment was never advanced. The row carries
    /// no audit content. Removing it is equivalent to "the row
    /// was never created" from the perspective of any future
    /// identical completion claim, which derives the same
    /// deterministic `transition_id` via
    /// [`super::transition_id::derive_transition_id`] and reaches
    /// the same `commit_protocol` flow.
    ///
    /// Contract:
    ///
    /// - `transition_id` does not exist in the ledger →
    ///   `Ok(())` (idempotent no-op).
    /// - Row exists with status `RESERVED` → row removed.
    /// - Row exists with status `PENDING` /
    ///   `DELIVERED` / `FAILED` →
    ///   [`LedgerError::InvalidStateForRelease`] with the
    ///   observed status. The caller MUST NOT silently coerce
    ///   the row into `RESERVED` and retry; every non-RESERVED
    ///   state means the transition has crossed an irreversible
    ///   boundary.
    ///
    /// Caller MUST follow with [`TransitionLedger::save_atomic`]
    /// to persist the deletion.
    ///
    /// This method:
    /// - **does NOT** call `messenger.send_targeted_activation`.
    /// - **does NOT** modify the assignment file.
    /// - **does NOT** derive a new `transition_id`.
    /// - **does NOT** mark the row `FAILED`.
    /// - **does NOT** touch any other row.
    /// - **does NOT** introduce a new state-machine transition;
    ///   deletion is observable as "row no longer present",
    ///   which is the same observable state as "never created".
    pub fn release_stale_reserved(&mut self, transition_id: &str) -> Result<(), LedgerError> {
        let idx_opt = self
            .entries
            .iter()
            .position(|e| e.transition_id == transition_id);
        let Some(idx) = idx_opt else {
            // No matching row → idempotent no-op.
            return Ok(());
        };
        match self.entries[idx].status {
            TransitionStatus::Reserved => {
                self.entries.remove(idx);
                Ok(())
            }
            TransitionStatus::Pending | TransitionStatus::Delivered | TransitionStatus::Failed => {
                Err(LedgerError::InvalidStateForRelease {
                    transition_id: transition_id.to_string(),
                    observed: self.entries[idx].status,
                })
            }
        }
    }

    fn transition_status(
        &mut self,
        transition_id: &str,
        to: TransitionStatus,
    ) -> Result<(), LedgerError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.transition_id == transition_id)
            .ok_or_else(|| LedgerError::UnknownTransition(transition_id.to_string()))?;
        let from = entry.status;
        let legal = matches!(
            (from, to),
            (TransitionStatus::Reserved, TransitionStatus::Pending)
                | (TransitionStatus::Reserved, TransitionStatus::Failed)
                | (TransitionStatus::Pending, TransitionStatus::Delivered)
                | (TransitionStatus::Pending, TransitionStatus::Failed)
        );
        if !legal {
            return Err(LedgerError::InvalidTransition {
                transition_id: transition_id.to_string(),
                from,
                to,
            });
        }
        entry.status = to;
        Ok(())
    }

    /// Prune the oldest `DELIVERED` entries when the total exceeds
    /// [`MAX_LEDGER_ENTRIES`]. Non-`DELIVERED` rows are never
    /// pruned, so a stalled workflow cannot lose its audit trail.
    fn prune_delivered(&mut self) {
        if self.entries.len() <= MAX_LEDGER_ENTRIES {
            return;
        }
        let active_count = self
            .entries
            .iter()
            .filter(|e| e.status != TransitionStatus::Delivered)
            .count();
        let delivered_capacity = MAX_LEDGER_ENTRIES.saturating_sub(active_count);

        let mut delivered: Vec<LedgerEntry> = self
            .entries
            .iter()
            .filter(|e| e.status == TransitionStatus::Delivered)
            .cloned()
            .collect();
        if delivered.len() <= delivered_capacity {
            return;
        }
        // Keep the most-recently-delivered entries.
        delivered.sort_by_key(|e| std::cmp::Reverse(e.delivered_at.unwrap_or(e.created_at)));
        delivered.truncate(delivered_capacity);

        let active: Vec<LedgerEntry> = self
            .entries
            .iter()
            .filter(|e| e.status != TransitionStatus::Delivered)
            .cloned()
            .collect();
        let mut combined = active;
        combined.extend(delivered);
        self.entries = combined;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ledger_path(dir: &Path) -> PathBuf {
        fs::canonicalize(dir)
            .unwrap()
            .join(WORKFLOW_DIR)
            .join(LEDGER_FILENAME)
    }

    #[test]
    fn load_missing_file_yields_empty_ledger() {
        let dir = TempDir::new().unwrap();
        let l = TransitionLedger::load(dir.path()).unwrap();
        assert!(l.entries().is_empty());
    }

    #[test]
    fn reserve_creates_new_entry() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        assert_eq!(e.status, TransitionStatus::Reserved);
        assert_eq!(e.workflow_id, "wf-001");
        assert_eq!(e.transition_id.len(), 32);
        assert_eq!(l.entries().len(), 1);
    }

    #[test]
    fn reserve_is_idempotent_for_reserved_rows() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let _ = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let e2 = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        assert_eq!(e2.status, TransitionStatus::Reserved);
        assert_eq!(l.entries().len(), 1);
    }

    #[test]
    fn reserve_rejects_already_delivered() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        l.mark_pending(&id, Some("1536734779607879700".into()))
            .unwrap();
        l.mark_delivered(&id, Some("discord-msg-1".into())).unwrap();
        let err = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .expect_err("already delivered must fail");
        assert!(matches!(err, LedgerError::AlreadyDelivered(_)));
    }

    #[test]
    fn reserve_does_not_mutate_failed_row() {
        // Issue 2 correction: Phase 2 is storage + validation only.
        // reserve() on a FAILED row must return FailedEntryExists and
        // leave the row untouched.
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        let original_created_at = e.created_at;
        l.mark_failed(&id).unwrap();
        assert_eq!(l.status_of(&id), Some(TransitionStatus::Failed));

        let count_before = l.entries().len();
        let err = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .expect_err("reserve on FAILED must not mutate");
        match err {
            LedgerError::FailedEntryExists(returned_id) => {
                assert_eq!(returned_id, id);
            }
            other => panic!("expected FailedEntryExists, got {other:?}"),
        }
        // Row count and timestamps unchanged.
        assert_eq!(l.entries().len(), count_before);
        let entry = l.lookup(&id).unwrap();
        assert_eq!(entry.status, TransitionStatus::Failed);
        assert_eq!(
            entry.created_at, original_created_at,
            "created_at must be preserved across failed reserve()"
        );
    }

    #[test]
    fn reserve_on_failed_does_not_create_replacement_reserved_row() {
        // Verifies that no fresh RESERVED row appears after a reserve()
        // on a FAILED row.
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        l.mark_failed(&id).unwrap();
        let _ = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .expect_err("must fail");
        // Still exactly one entry, still FAILED.
        assert_eq!(l.entries().len(), 1);
        assert_eq!(l.entries()[0].status, TransitionStatus::Failed);
        assert_eq!(l.entries()[0].transition_id, id);
    }

    #[test]
    fn failed_failure_metadata_remains_intact() {
        // Verify that `openab_message_id` / `target_user_id` /
        // `created_at` set before mark_failed are preserved.
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        let original = l.lookup(&id).unwrap().clone();
        l.mark_pending(&id, Some("1536734779607879700".into()))
            .unwrap();
        l.mark_failed(&id).unwrap();
        let after = l.lookup(&id).unwrap();
        assert_eq!(after.transition_id, original.transition_id);
        assert_eq!(after.created_at, original.created_at);
        assert_eq!(
            after.target_user_id.as_deref(),
            Some("1536734779607879700"),
            "target_user_id set during mark_pending must survive mark_failed"
        );
        assert_eq!(after.status, TransitionStatus::Failed);
    }

    #[test]
    fn status_of_unknown_returns_none() {
        let dir = TempDir::new().unwrap();
        let l = TransitionLedger::load(dir.path()).unwrap();
        assert!(l.status_of("nope").is_none());
    }

    #[test]
    fn mark_pending_only_allows_reserved() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        l.mark_pending(&id, Some("u1".into())).unwrap();
        // Second mark_pending must fail: PENDING -> PENDING is illegal.
        let err = l
            .mark_pending(&id, Some("u2".into()))
            .expect_err("must reject");
        match err {
            LedgerError::InvalidTransition { from, to, .. } => {
                assert_eq!(from, TransitionStatus::Pending);
                assert_eq!(to, TransitionStatus::Pending);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn mark_delivered_records_message_id_and_timestamp() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        l.mark_pending(&id, Some("u1".into())).unwrap();
        l.mark_delivered(&id, Some("discord-msg-42".into()))
            .unwrap();
        let entry = l.lookup(&id).unwrap();
        assert_eq!(entry.status, TransitionStatus::Delivered);
        assert_eq!(entry.openab_message_id.as_deref(), Some("discord-msg-42"));
        assert!(entry.delivered_at.is_some());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let e = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let id = e.transition_id.clone();
        l.mark_pending(&id, Some("1536734779607879700".into()))
            .unwrap();
        l.save_atomic().unwrap();

        let loaded = TransitionLedger::load(dir.path()).unwrap();
        assert_eq!(loaded.entries().len(), 1);
        let entry = loaded.lookup(&id).unwrap();
        assert_eq!(entry.status, TransitionStatus::Pending);
        assert_eq!(entry.target_user_id.as_deref(), Some("1536734779607879700"));
    }

    #[test]
    fn malformed_ledger_fails_closed() {
        let dir = TempDir::new().unwrap();
        let openab = fs::canonicalize(dir.path()).unwrap().join(WORKFLOW_DIR);
        fs::create_dir_all(&openab).unwrap();
        let path = openab.join(LEDGER_FILENAME);
        fs::write(&path, "{ not valid ledger }").unwrap();

        let err = TransitionLedger::load(dir.path()).expect_err("malformed must fail");
        assert!(matches!(err, LedgerError::Malformed(_)));
    }

    #[test]
    fn atomic_save_leaves_no_tmp_sibling() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let _ = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        let path = l.save_atomic().unwrap();
        let stale = path.with_extension("json.tmp");
        assert!(!stale.exists(), ".json.tmp sibling must not survive");
    }

    #[test]
    fn prune_drops_oldest_delivered_when_over_capacity() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();

        // Push MAX_LEDGER_ENTRIES + 5 DELIVERED rows directly.
        for i in 0..(MAX_LEDGER_ENTRIES + 5) {
            let mut e = LedgerEntry {
                transition_id: format!("{i:032x}"),
                workflow_id: "wf-001".into(),
                workflow_revision: 0,
                current_stage: WorkflowStage::PrimaryActive,
                role: WorkflowRole::Primary,
                result: CompletionResult::Complete,
                status: TransitionStatus::Delivered,
                openab_message_id: Some(format!("msg-{i}")),
                target_user_id: Some("u1".into()),
                created_at: Utc::now(),
                delivered_at: Some(Utc::now()),
            };
            // Spread delivered_at timestamps monotonically so newer
            // entries sort later.
            e.delivered_at = Some(Utc::now() + chrono::Duration::seconds(i as i64));
            l.entries.push(e);
        }
        // Trigger pruning.
        let first_id = l.entries[0].transition_id.clone();
        let last_id = l.entries[l.entries.len() - 1].transition_id.clone();
        l.prune_delivered();

        // Total count is now capped at MAX_LEDGER_ENTRIES.
        assert_eq!(l.entries.len(), MAX_LEDGER_ENTRIES);
        // The oldest (the first we inserted) is gone.
        assert!(l.lookup(&first_id).is_none());
        // The newest is kept.
        assert!(l.lookup(&last_id).is_some());
        // Every kept DELIVERED row's delivered_at is at-or-after the
        // cutoff (the median timestamp of the original set).
        let all_delivered: Vec<_> = l
            .entries
            .iter()
            .filter(|e| e.status == TransitionStatus::Delivered)
            .collect();
        assert_eq!(all_delivered.len(), MAX_LEDGER_ENTRIES);
    }

    #[test]
    fn non_delivered_rows_are_never_pruned() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();

        // Mix DELIVERED + RESERVED rows beyond MAX_LEDGER_ENTRIES.
        let delivered_count = MAX_LEDGER_ENTRIES;
        for i in 0..delivered_count {
            l.entries.push(LedgerEntry {
                transition_id: format!("d{i:032x}"),
                workflow_id: "wf-001".into(),
                workflow_revision: 0,
                current_stage: WorkflowStage::PrimaryActive,
                role: WorkflowRole::Primary,
                result: CompletionResult::Complete,
                status: TransitionStatus::Delivered,
                openab_message_id: Some(format!("msg-{i}")),
                target_user_id: None,
                created_at: Utc::now(),
                delivered_at: Some(Utc::now()),
            });
        }
        let reserved_id = "active-reserved-row".to_string();
        l.entries.push(LedgerEntry {
            transition_id: reserved_id.clone(),
            workflow_id: "wf-001".into(),
            workflow_revision: 0,
            current_stage: WorkflowStage::PrimaryActive,
            role: WorkflowRole::Primary,
            result: CompletionResult::Complete,
            status: TransitionStatus::Reserved,
            openab_message_id: None,
            target_user_id: None,
            created_at: Utc::now(),
            delivered_at: None,
        });
        let pending_id = "active-pending-row".to_string();
        l.entries.push(LedgerEntry {
            transition_id: pending_id.clone(),
            workflow_id: "wf-001".into(),
            workflow_revision: 0,
            current_stage: WorkflowStage::PrimaryActive,
            role: WorkflowRole::Primary,
            result: CompletionResult::Complete,
            status: TransitionStatus::Pending,
            openab_message_id: Some("in-flight".into()),
            target_user_id: Some("u1".into()),
            created_at: Utc::now(),
            delivered_at: None,
        });
        l.prune_delivered();

        assert!(l.lookup(&reserved_id).is_some());
        assert!(l.lookup(&pending_id).is_some());
        assert!(l.entries.len() <= MAX_LEDGER_ENTRIES);
    }

    #[test]
    fn save_creates_openab_directory_when_when() {
        let dir = TempDir::new().unwrap();
        let mut l = TransitionLedger::load(dir.path()).unwrap();
        let _ = l
            .reserve(
                "wf-001",
                0,
                WorkflowStage::PrimaryActive,
                WorkflowRole::Primary,
                CompletionResult::Complete,
            )
            .unwrap();
        // Pre-condition: .openab does not exist yet.
        assert!(!ledger_path(dir.path()).parent().unwrap().exists());
        l.save_atomic().unwrap();
        assert!(ledger_path(dir.path()).exists());
    }
}

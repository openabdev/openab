//! Project-local workflow assignment persistence for the OpenAB-native
//! three-agent coding workflow.
//!
//! # Canonical storage path
//!
//! ```text
//! <canonical_project_root>/.openab/workflow_assignment.json
//! ```
//!
//! OpenAB owns the `.openab` namespace and never reads or writes
//! `<project_root>/.agents/workflow_assignment.json` — that path belongs
//! to the legacy ai-workstation runtime, which the Phase 1 brief
//! explicitly excludes from this work.
//!
//! # Atomic write pattern
//!
//! Matches the existing OpenAB pool pattern at
//! `crates/openab-core/src/acp/pool.rs:668-686`:
//!
//! 1. Canonicalize `project_root` (rejects empty / non-existent /
//!    non-directory / non-absolute inputs).
//! 2. Create `<canonical_project_root>/.openab` if missing.
//! 3. Write to a sibling `<final>.json.tmp`.
//! 4. Flush the temp file.
//! 5. Rename the temp file onto the final path.
//!
//! A crash mid-write cannot leave a half-written file behind to be
//! mistaken for a real assignment on the next startup.
//!
//! # Fail-closed invariants
//!
//! - malformed JSON: returns [`AssignmentError::Malformed`]
//! - unsupported `schema_version`: returns [`AssignmentError::UnsupportedSchemaVersion`]
//! - missing required string field: returns [`AssignmentError::MissingField`]
//! - non-canonical `project_root`: returns [`AssignmentError::ProjectRootMismatch`]
//! - `defect_loop_count > SUPPORTED_DEFECT_LOOP_MAX`: returns
//!   [`AssignmentError::DefectLoopExceeded`]
//!
//! `load_assignment` MUST NEVER silently fall back to another project's
//! assignment. If canonical-path resolution fails or the file contents
//! disagree with the requested root, it returns an error.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::state::{WorkflowMode, WorkflowStage};

/// Schema version for the on-disk assignment format. Phase 1 ships
/// `v2`; older `v1` files (legacy ai-workstation format) are rejected
/// to keep the two namespaces strictly separate.
pub const SCHEMA_VERSION: &str = "v2";

/// Project-local workflow namespace directory. Owned by OpenAB.
pub const WORKFLOW_DIR: &str = ".openab";

/// Canonical assignment filename inside [`WORKFLOW_DIR`].
pub const ASSIGNMENT_FILENAME: &str = "workflow_assignment.json";

/// Canonical maximum value for `defect_loop_count`.
///
/// The bounded defect loop allows at most one `PRIMARY_CORRECTION_PENDING`
/// cycle before re-entering `VERIFIER_ACTIVE`. Phase 1 only persists and
/// validates the field; the increment lands in a later phase.
pub const SUPPORTED_DEFECT_LOOP_MAX: u32 = 1;

/// Compute the canonical assignment path for a project root.
///
/// Joins the canonicalized `project_root`, the OpenAB-owned
/// [`WORKFLOW_DIR`], and [`ASSIGNMENT_FILENAME`]. Canonicalization
/// collapses `"/a"`, `"/a/"`, `"/a/./"` into the same key, so a project
/// is identified by what it actually points at on disk rather than by
/// how the caller spelled it.
///
/// Returns [`AssignmentError`] if the path cannot be canonicalized.
pub fn assignment_path(project_root: &Path) -> Result<PathBuf, AssignmentError> {
    let canonical = canonicalize_root(project_root)?;
    Ok(canonical.join(WORKFLOW_DIR).join(ASSIGNMENT_FILENAME))
}

/// Canonical workflow assignment for the three-agent coding workflow.
///
/// Every field is trusted. Phase 1 validates the structural invariants
/// in [`WorkflowAssignment::validate`] at load and save time so a
/// hand-edited or partial file cannot poison the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAssignment {
    /// Schema version, pinned at `"v2"` for Phase 1.
    pub schema_version: String,
    /// Stable workflow identifier (UUIDv7 or a Tech-Lead-supplied id).
    pub workflow_id: String,
    /// Stable project identifier (matches the `project_id` from the
    /// inbound `ProjectContext`).
    pub project_id: String,
    /// Canonical absolute path of the project root. Validated at
    /// load/save time so callers cannot smuggle a non-canonical or
    /// cross-project path into the state machine.
    pub project_root: PathBuf,
    /// Operating mode. Phase 1 stores the field; degraded routing
    /// behaviour lands later.
    pub mode: WorkflowMode,
    /// Logical agent identity holding the PRIMARY slot.
    pub primary: String,
    /// Logical agent identity holding the VERIFIER slot.
    pub verifier: String,
    /// Logical agent identity holding the FINAL_REVIEWER slot.
    pub final_reviewer: String,
    /// Current canonical stage. Every transition runs through
    /// [`super::state::legal_next_stage`].
    pub state: WorkflowStage,
    /// OpenAB-owned monotonic counter. Increments on every committed
    /// transition; the new value produces a fresh
    /// [`super::transition_id::derive_transition_id`] for the same
    /// `(stage, role, result)` tuple.
    pub workflow_revision: u64,
    /// Number of bounded defect loops consumed so far. Capped at
    /// [`SUPPORTED_DEFECT_LOOP_MAX`] (1) by [`WorkflowAssignment::validate`].
    pub defect_loop_count: u32,
    /// Latest Tech-Lead-selected workflow response language. Preserved
    /// across all transitions.
    pub language: String,
    /// Discord thread channel id (threads are channels in Discord).
    pub thread_id: String,
    /// The transition_id of the most recent committed transition, or
    /// `None` for a freshly-created assignment.
    pub last_transition_id: Option<String>,
    /// Discord message id of the most recent targeted handoff delivery,
    /// or `None` if no handoff has been sent.
    pub last_delivery_message_id: Option<String>,
    /// Logical agent identities declared unavailable by the Tech Lead.
    /// Empty in `THREE_AGENT` mode.
    pub unavailable_agents: Vec<String>,
    /// Operator that authorized this workflow (e.g. `"Tech Lead"`).
    pub authorized_by: String,
    /// Free-form reason text recorded at creation or reassignment.
    pub reason: String,
    /// Timestamp the assignment was first created.
    pub created_at: DateTime<Utc>,
    /// Timestamp of the most recent persisted update. `save_assignment_atomic`
    /// refreshes this field at write time.
    pub updated_at: DateTime<Utc>,
}

impl WorkflowAssignment {
    /// Construct a new assignment with sensible defaults for the
    /// string fields. Phase 1 callers are responsible for filling in
    /// `language`, `thread_id`, `authorized_by`, and `reason`.
    pub fn new(
        workflow_id: impl Into<String>,
        project_id: impl Into<String>,
        project_root: PathBuf,
        primary: impl Into<String>,
        verifier: impl Into<String>,
        final_reviewer: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            workflow_id: workflow_id.into(),
            project_id: project_id.into(),
            project_root,
            mode: WorkflowMode::default(),
            primary: primary.into(),
            verifier: verifier.into(),
            final_reviewer: final_reviewer.into(),
            state: WorkflowStage::PrimaryActive,
            workflow_revision: 0,
            defect_loop_count: 0,
            language: "en".to_string(),
            thread_id: String::new(),
            last_transition_id: None,
            last_delivery_message_id: None,
            unavailable_agents: Vec::new(),
            authorized_by: String::new(),
            reason: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Validate structural invariants. Phase 1 enforces the cheap
    /// shape checks; transition-side checks (legal-next-stage,
    /// expected-role-for-stage, terminal-state rejection) live in
    /// [`super::state::legal_next_stage`] and Phase 2's validator.
    pub fn validate(&self) -> Result<(), AssignmentError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(AssignmentError::UnsupportedSchemaVersion(
                self.schema_version.clone(),
            ));
        }
        require_non_empty(&self.workflow_id, "workflow_id")?;
        require_non_empty(&self.project_id, "project_id")?;
        if self.project_root.as_os_str().is_empty() {
            return Err(AssignmentError::MissingField("project_root"));
        }
        if !self.project_root.is_absolute() {
            return Err(AssignmentError::ProjectRootNotAbsolute(
                self.project_root.clone(),
            ));
        }
        require_non_empty(&self.primary, "primary")?;
        require_non_empty(&self.verifier, "verifier")?;
        require_non_empty(&self.final_reviewer, "final_reviewer")?;
        if self.defect_loop_count > SUPPORTED_DEFECT_LOOP_MAX {
            return Err(AssignmentError::DefectLoopExceeded {
                count: self.defect_loop_count,
                max: SUPPORTED_DEFECT_LOOP_MAX,
            });
        }
        Ok(())
    }
}

fn require_non_empty(field: &str, name: &'static str) -> Result<(), AssignmentError> {
    if field.is_empty() {
        Err(AssignmentError::MissingField(name))
    } else {
        Ok(())
    }
}

/// Failure modes for assignment load/save/validate.
///
/// All variants are fail-closed: a non-`None` `Err` is the signal that
/// no state change has been persisted.
#[derive(Debug)]
pub enum AssignmentError {
    /// `project_root` was an empty path.
    EmptyProjectRoot,
    /// `project_root` could not be canonicalized (does not exist, I/O
    /// error, permission denied, etc.).
    ProjectRootUnreadable { path: PathBuf, reason: String },
    /// `project_root` canonicalized to a non-directory (file, symlink
    /// to a file, etc.).
    ProjectRootNotDirectory(PathBuf),
    /// `project_root` was not absolute after canonicalization.
    ProjectRootNotAbsolute(PathBuf),
    /// The assignment file's `project_root` did not match the canonical
    /// root of the directory it was loaded from. Fails closed to prevent
    /// cross-project fallback.
    ProjectRootMismatch {
        file_value: PathBuf,
        canonical: PathBuf,
    },
    /// JSON parse error or shape mismatch.
    Malformed(String),
    /// `schema_version` was not `"v2"`.
    UnsupportedSchemaVersion(String),
    /// A required string field was empty.
    MissingField(&'static str),
    /// `defect_loop_count` exceeded [`SUPPORTED_DEFECT_LOOP_MAX`].
    DefectLoopExceeded { count: u32, max: u32 },
    /// Filesystem I/O error.
    Io { path: PathBuf, reason: String },
}

impl std::fmt::Display for AssignmentError {
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
            Self::ProjectRootMismatch {
                file_value,
                canonical,
            } => write!(
                f,
                "assignment project_root {file_value:?} does not match canonical {canonical:?}"
            ),
            Self::Malformed(reason) => write!(f, "malformed assignment JSON: {reason}"),
            Self::UnsupportedSchemaVersion(v) => {
                write!(f, "unsupported schema_version {v:?}")
            }
            Self::MissingField(name) => write!(f, "missing required field {name:?}"),
            Self::DefectLoopExceeded { count, max } => write!(
                f,
                "defect_loop_count {count} exceeds supported maximum {max}"
            ),
            Self::Io { path, reason } => {
                write!(f, "workflow assignment I/O error at {path:?}: {reason}")
            }
        }
    }
}

impl std::error::Error for AssignmentError {}

/// Canonicalize `p` for any workflow-module caller (assignment,
/// ledger, recovery). Visible to sibling modules in `workflow` but
/// hidden from the rest of the crate.
pub(super) fn canonicalize_root_for_workflow(p: &Path) -> Result<PathBuf, AssignmentError> {
    canonicalize_root(p)
}

fn canonicalize_root(p: &Path) -> Result<PathBuf, AssignmentError> {
    if p.as_os_str().is_empty() {
        return Err(AssignmentError::EmptyProjectRoot);
    }
    let canonical = fs::canonicalize(p).map_err(|e| AssignmentError::ProjectRootUnreadable {
        path: p.to_path_buf(),
        reason: e.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(AssignmentError::ProjectRootNotDirectory(canonical));
    }
    if !canonical.is_absolute() {
        return Err(AssignmentError::ProjectRootNotAbsolute(canonical));
    }
    Ok(canonical)
}

/// Load a workflow assignment from
/// `<project_root>/.openab/workflow_assignment.json`.
///
/// Returns:
/// - `Ok(Some(assignment))` if the file exists and parses,
/// - `Ok(None)` if the file does not exist (this is not an error;
///   the caller can decide whether a fresh assignment should be
///   created),
/// - `Err(AssignmentError)` if the project_root is unreadable, the
///   JSON is malformed, or any invariant fails.
///
/// `load_assignment` MUST NEVER silently fall back to another
/// project's assignment. If canonical-path resolution fails, or if
/// the file's `project_root` disagrees with the canonical root of the
/// directory it lives in, it returns an error.
pub fn load_assignment(project_root: &Path) -> Result<Option<WorkflowAssignment>, AssignmentError> {
    let canonical = canonicalize_root(project_root)?;
    let path = canonical.join(WORKFLOW_DIR).join(ASSIGNMENT_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path).map_err(|e| AssignmentError::Io {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    let assignment: WorkflowAssignment =
        serde_json::from_str(&data).map_err(|e| AssignmentError::Malformed(e.to_string()))?;
    // The on-disk project_root must match the canonical root we just
    // computed. Anything else means the file belongs to a different
    // project and we must fail closed rather than silently rebind.
    if assignment.project_root != canonical {
        return Err(AssignmentError::ProjectRootMismatch {
            file_value: assignment.project_root.clone(),
            canonical,
        });
    }
    assignment.validate()?;
    Ok(Some(assignment))
}

/// Save a workflow assignment atomically to
/// `<project_root>/.openab/workflow_assignment.json`.
///
/// Atomic write pattern (mirrors `pool.rs:668-686`):
///
/// 1. Canonicalize `project_root`.
/// 2. Refresh `assignment.updated_at` and overwrite
///    `assignment.project_root` with the canonical path.
/// 3. Run [`WorkflowAssignment::validate`].
/// 4. Create `<canonical_project_root>/.openab` if missing.
/// 5. Write to a sibling `<final>.json.tmp`.
/// 6. Flush the temp file.
/// 7. Rename the temp file onto the final path.
///
/// Returns the absolute final path on success.
///
/// NEVER reads or writes `<project_root>/.agents/workflow_assignment.json`.
pub fn save_assignment_atomic(
    project_root: &Path,
    assignment: &WorkflowAssignment,
) -> Result<PathBuf, AssignmentError> {
    // Test-only failpoint: Phase 4.2 fault-injection tests arm this
    // counter to force `save_assignment_atomic` to fail and exercise
    // the recovery logic. Production builds compile this arm to a
    // true branch that never executes.
    #[cfg(test)]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FAIL_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
        static FAIL_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = FAIL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        let target = FAIL_AFTER.load(Ordering::SeqCst);
        if n == target {
            return Err(AssignmentError::Io {
                path: project_root.to_path_buf(),
                reason: "test_injected:save_assignment_atomic".to_string(),
            });
        }
    }
    let canonical = canonicalize_root(project_root)?;

    // Build the to-save snapshot. Project_root is forced to the
    // canonical path so the on-disk field never drifts from what
    // canonicalize() would produce; updated_at is refreshed to "now".
    let mut to_save = assignment.clone();
    to_save.project_root = canonical.clone();
    to_save.updated_at = Utc::now();
    to_save.validate()?;

    let dir = canonical.join(WORKFLOW_DIR);
    fs::create_dir_all(&dir).map_err(|e| AssignmentError::Io {
        path: dir.clone(),
        reason: format!("create_dir_all: {e}"),
    })?;

    let final_path = dir.join(ASSIGNMENT_FILENAME);
    let tmp_path = final_path.with_extension("json.tmp");

    let data = serde_json::to_string_pretty(&to_save)
        .map_err(|e| AssignmentError::Malformed(e.to_string()))?;

    {
        let mut f = fs::File::create(&tmp_path).map_err(|e| AssignmentError::Io {
            path: tmp_path.clone(),
            reason: format!("create tmp: {e}"),
        })?;
        f.write_all(data.as_bytes())
            .map_err(|e| AssignmentError::Io {
                path: tmp_path.clone(),
                reason: format!("write tmp: {e}"),
            })?;
        f.flush().map_err(|e| AssignmentError::Io {
            path: tmp_path.clone(),
            reason: format!("flush tmp: {e}"),
        })?;
    }
    fs::rename(&tmp_path, &final_path).map_err(|e| AssignmentError::Io {
        path: final_path.clone(),
        reason: format!("rename tmp onto final: {e}"),
    })?;

    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a fully-populated, valid assignment against `project_root`.
    fn sample(project_root: PathBuf) -> WorkflowAssignment {
        let mut a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            project_root,
            "ArthurClaude",
            "ArthurCodex",
            "ArthurGemini",
        );
        a.language = "zh-TW".into();
        a.thread_id = "1536735741642547262".into();
        a.authorized_by = "Tech Lead".into();
        a.reason = "phase-1 sample".into();
        a
    }

    /// Strip the trailing newline from a path so we can compare to the
    /// canonicalized form returned by `fs::canonicalize`.
    fn canonical(p: &Path) -> PathBuf {
        fs::canonicalize(p).expect("canonicalize")
    }

    // ---- Test 6: save/load round trip ----

    #[test]
    fn save_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let a = sample(dir.path().to_path_buf());
        let written = save_assignment_atomic(dir.path(), &a).expect("save");
        assert!(written.exists());

        let loaded = load_assignment(dir.path()).expect("load");
        let loaded = loaded.expect("assignment must exist");
        assert_eq!(loaded.workflow_id, "wf-001");
        assert_eq!(loaded.project_id, "openab");
        assert_eq!(loaded.primary, "ArthurClaude");
        assert_eq!(loaded.verifier, "ArthurCodex");
        assert_eq!(loaded.final_reviewer, "ArthurGemini");
        assert_eq!(loaded.state, WorkflowStage::PrimaryActive);
        assert_eq!(loaded.workflow_revision, 0);
        assert_eq!(loaded.defect_loop_count, 0);
        assert_eq!(loaded.language, "zh-TW");
        assert_eq!(loaded.thread_id, "1536735741642547262");
        assert_eq!(loaded.authorized_by, "Tech Lead");
        assert_eq!(loaded.unavailable_agents, Vec::<String>::new());
        // project_root is forced canonical on save.
        assert_eq!(loaded.project_root, canonical(dir.path()));
    }

    // ---- Test 7: canonical project_root ----

    #[test]
    fn canonical_project_root_collapse() {
        let dir = TempDir::new().unwrap();
        // Build a path with redundant components and trailing slash.
        let messy = dir.path().join(".").join("./");
        let a = sample(messy);
        let _ = save_assignment_atomic(dir.path(), &a).expect("save");

        // Read the raw JSON and confirm the on-disk project_root is the
        // canonical absolute path with no `./` residue.
        let path = canonical(dir.path())
            .join(WORKFLOW_DIR)
            .join(ASSIGNMENT_FILENAME);
        let raw = fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let stored = v["project_root"].as_str().unwrap();
        assert!(
            !stored.contains("//"),
            "stored path must be canonical: {stored:?}"
        );
        assert!(
            !stored.contains("/./"),
            "stored path must drop /./: {stored:?}"
        );
        assert!(
            Path::new(stored).is_absolute(),
            "stored path must be absolute: {stored:?}"
        );
        assert_eq!(Path::new(stored), canonical(dir.path()).as_path());
    }

    // ---- Test 8: .openab path used ----

    #[test]
    fn openab_path_is_used() {
        let dir = TempDir::new().unwrap();
        let a = sample(dir.path().to_path_buf());
        let written = save_assignment_atomic(dir.path(), &a).expect("save");

        // The written file must live under <root>/.openab/, not under
        // .agents/ or anywhere else.
        assert!(
            written.starts_with(canonical(dir.path()).join(WORKFLOW_DIR)),
            "file must live under {WORKFLOW_DIR:?}, got {written:?}"
        );
        assert!(!written.to_string_lossy().contains(".agents"));

        // The directory must exist for next-load.
        assert!(written.parent().unwrap().is_dir());
    }

    // ---- Test 9: .agents path untouched ----

    #[test]
    fn agents_path_is_never_written() {
        let dir = TempDir::new().unwrap();
        // Pre-create a stale .agents/workflow_assignment.json that
        // belongs to some other system. OpenAB must never overwrite it.
        let legacy = dir.path().join(".agents").join("workflow_assignment.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let legacy_data = r#"{"schema_version":"v1","workflow_id":"legacy"}"#;
        fs::write(&legacy, legacy_data).unwrap();

        let a = sample(dir.path().to_path_buf());
        save_assignment_atomic(dir.path(), &a).expect("save");

        // The legacy file is byte-for-byte unchanged.
        let after = fs::read_to_string(&legacy).unwrap();
        assert_eq!(after, legacy_data, "OpenAB must not touch .agents/");
    }

    // ---- Test 10: malformed JSON fails closed ----

    #[test]
    fn malformed_json_fails_closed() {
        let dir = TempDir::new().unwrap();
        let openab = canonical(dir.path()).join(WORKFLOW_DIR);
        fs::create_dir_all(&openab).unwrap();
        let path = openab.join(ASSIGNMENT_FILENAME);
        fs::write(&path, "{ not valid json }").unwrap();

        let err = load_assignment(dir.path()).expect_err("malformed must fail");
        match err {
            AssignmentError::Malformed(_) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    // ---- Test 11: invalid state fails closed ----

    #[test]
    fn invalid_state_fails_closed() {
        let dir = TempDir::new().unwrap();
        let openab = canonical(dir.path()).join(WORKFLOW_DIR);
        fs::create_dir_all(&openab).unwrap();
        let path = openab.join(ASSIGNMENT_FILENAME);
        // state = "BOGUS_STATE" is not in the canonical set.
        let body = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "workflow_id": "wf-001",
            "project_id": "openab",
            "project_root": canonical(dir.path()),
            "mode": "THREE_AGENT",
            "primary": "ArthurClaude",
            "verifier": "ArthurCodex",
            "final_reviewer": "ArthurGemini",
            "state": "BOGUS_STATE",
            "workflow_revision": 0,
            "defect_loop_count": 0,
            "language": "en",
            "thread_id": "",
            "last_transition_id": null,
            "last_delivery_message_id": null,
            "unavailable_agents": [],
            "authorized_by": "Tech Lead",
            "reason": "",
            "created_at": "2026-08-18T00:00:00Z",
            "updated_at": "2026-08-18T00:00:00Z"
        });
        fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).unwrap();

        let err = load_assignment(dir.path()).expect_err("invalid state must fail");
        // serde rejects the unknown enum variant before validate() runs,
        // so we get Malformed at the JSON layer.
        assert!(
            matches!(err, AssignmentError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    // ---- Test 12: defect_loop_count defaults correctly ----

    #[test]
    fn defect_loop_count_defaults_to_zero() {
        let dir = TempDir::new().unwrap();
        let a = WorkflowAssignment::new(
            "wf-001",
            "openab",
            dir.path().to_path_buf(),
            "ArthurClaude",
            "ArthurCodex",
            "ArthurGemini",
        );
        assert_eq!(a.defect_loop_count, 0);

        // Save+load preserves the default.
        save_assignment_atomic(dir.path(), &a).unwrap();
        let loaded = load_assignment(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.defect_loop_count, 0);
    }

    // ---- Test 13: defect_loop_count > supported maximum rejected ----

    #[test]
    fn defect_loop_count_exceeding_max_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut a = sample(dir.path().to_path_buf());
        a.defect_loop_count = SUPPORTED_DEFECT_LOOP_MAX + 1;
        let err = a.validate().expect_err("must reject");
        match err {
            AssignmentError::DefectLoopExceeded { count, max } => {
                assert_eq!(count, SUPPORTED_DEFECT_LOOP_MAX + 1);
                assert_eq!(max, SUPPORTED_DEFECT_LOOP_MAX);
            }
            other => panic!("expected DefectLoopExceeded, got {other:?}"),
        }
        // save must also reject, not just validate().
        let err = save_assignment_atomic(dir.path(), &a).expect_err("save must reject");
        assert!(matches!(err, AssignmentError::DefectLoopExceeded { .. }));

        // On-disk file (if any) is unchanged.
        let openab = canonical(dir.path()).join(WORKFLOW_DIR);
        assert!(
            !openab.join(ASSIGNMENT_FILENAME).exists(),
            "save must not have created a file when validation failed"
        );
    }

    // ---- Test 14: atomic replacement leaves valid final JSON ----

    #[test]
    fn atomic_replacement_leaves_valid_final_json() {
        let dir = TempDir::new().unwrap();
        let a1 = sample(dir.path().to_path_buf());
        save_assignment_atomic(dir.path(), &a1).unwrap();

        // Overwrite with a different revision.
        let mut a2 = a1.clone();
        a2.workflow_revision = 7;
        save_assignment_atomic(dir.path(), &a2).unwrap();

        // No stray .json.tmp siblings remain.
        let openab = canonical(dir.path()).join(WORKFLOW_DIR);
        let stale = openab.join(format!("{ASSIGNMENT_FILENAME}.tmp"));
        assert!(
            !stale.exists(),
            ".json.tmp sibling must not survive: {stale:?}"
        );

        // Final file parses and matches the second save.
        let loaded = load_assignment(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.workflow_revision, 7);
        // `updated_at` is refreshed by save_assignment_atomic at write
        // time (see the docstring on `updated_at`), so compare every
        // other field directly.
        assert_eq!(loaded.workflow_id, a2.workflow_id);
        assert_eq!(loaded.project_id, a2.project_id);
        assert_eq!(loaded.primary, a2.primary);
        assert_eq!(loaded.verifier, a2.verifier);
        assert_eq!(loaded.final_reviewer, a2.final_reviewer);
        assert_eq!(loaded.state, a2.state);
        assert_eq!(loaded.defect_loop_count, a2.defect_loop_count);
        assert_eq!(loaded.language, a2.language);
        assert_eq!(loaded.thread_id, a2.thread_id);
        // The refreshed updated_at must be at-or-after the second save's
        // snapshot time (we cannot predict the exact clock, only the
        // monotonic order between writes).
        assert!(
            loaded.updated_at >= a2.updated_at,
            "save must refresh updated_at monotonically: loaded={} snapshot={}",
            loaded.updated_at,
            a2.updated_at
        );
        assert!(
            loaded.updated_at >= a1.updated_at,
            "second save must move updated_at forward: loaded={} first={}",
            loaded.updated_at,
            a1.updated_at
        );

        // The on-disk JSON parses as canonical WorkflowAssignment JSON.
        let raw = fs::read_to_string(openab.join(ASSIGNMENT_FILENAME)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["workflow_revision"], 7);
    }

    // ---- Test 15: two project roots remain isolated ----

    #[test]
    fn two_project_roots_remain_isolated() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let a = sample(dir_a.path().to_path_buf());
        let mut b = sample(dir_b.path().to_path_buf());
        b.workflow_id = "wf-b".to_string();
        b.project_id = "openab-b".to_string();

        save_assignment_atomic(dir_a.path(), &a).unwrap();
        save_assignment_atomic(dir_b.path(), &b).unwrap();

        // Loading dir A returns the A assignment.
        let loaded_a = load_assignment(dir_a.path()).unwrap().unwrap();
        assert_eq!(loaded_a.workflow_id, "wf-001");

        // Loading dir B returns the B assignment.
        let loaded_b = load_assignment(dir_b.path()).unwrap().unwrap();
        assert_eq!(loaded_b.workflow_id, "wf-b");

        // The on-disk files do not cross.
        let path_a = canonical(dir_a.path())
            .join(WORKFLOW_DIR)
            .join(ASSIGNMENT_FILENAME);
        let path_b = canonical(dir_b.path())
            .join(WORKFLOW_DIR)
            .join(ASSIGNMENT_FILENAME);
        assert!(path_a.exists());
        assert!(path_b.exists());
        assert_ne!(path_a, path_b);

        // A file with a project_root that disagrees with the directory
        // it lives in is rejected, not silently rebound.
        let tampered = dir_a.path().join(WORKFLOW_DIR).join(ASSIGNMENT_FILENAME);
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&tampered).unwrap()).unwrap();
        // Pretend the file thinks its project_root is dir_b.
        raw["project_root"] =
            serde_json::Value::String(canonical(dir_b.path()).to_string_lossy().into_owned());
        fs::write(&tampered, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
        let err = load_assignment(dir_a.path()).expect_err("mismatch must fail");
        assert!(
            matches!(err, AssignmentError::ProjectRootMismatch { .. }),
            "expected ProjectRootMismatch, got {err:?}"
        );
    }

    // ---- Bonus: validation rejects empty required fields ----

    #[test]
    fn validate_rejects_empty_workflow_id() {
        let dir = TempDir::new().unwrap();
        let mut a = sample(dir.path().to_path_buf());
        a.workflow_id.clear();
        assert!(matches!(
            a.validate().expect_err("empty workflow_id"),
            AssignmentError::MissingField("workflow_id")
        ));
    }

    #[test]
    fn save_creates_openab_directory_when_missing() {
        let dir = TempDir::new().unwrap();
        // Pre-condition: .openab does not exist.
        assert!(!dir.path().join(WORKFLOW_DIR).exists());
        let a = sample(dir.path().to_path_buf());
        let written = save_assignment_atomic(dir.path(), &a).unwrap();
        assert!(written.parent().unwrap().is_dir());
        assert!(written.exists());
    }
}

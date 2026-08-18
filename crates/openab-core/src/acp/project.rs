//! Transport-neutral project identity passed into ACP session creation.
//!
//! An ACP session created with `Some(ProjectContext)` is permanently bound to
//! one `(project_id, project_root)` tuple. An existing session for the same
//! `thread_id` whose stored binding differs from the incoming context is
//! rejected explicitly by [`SessionPool::get_or_create`] rather than
//! silently reused (see ADR §4.5 + workflow
//! `20260818-openab-project-scoped-acp-session-bootstrap`).
//!
//! [`SessionPool::get_or_create`]: crate::acp::pool::SessionPool::get_or_create
//!
//! # Anonymous contexts
//!
//! [`ProjectContext::anonymous`] produces a context with an empty
//! `project_id`. Anonymous contexts contribute a workspace path but do NOT
//! pin a session to a `project_id` and do NOT trigger mismatch checks. This
//! keeps the legacy `[[ws:@alias]]` directive path compatible without
//! forcing every workspace hint to carry a `project_id`.
//!
//! # Validation
//!
//! [`ProjectContext::validate`] requires `project_root` to be an existing
//! directory and returns the canonical absolute path so callers can store
//! and compare without re-canonicalizing on every lookup. Canonicalization
//! collapses `"/a"`, `"/a/"`, and `"/a/./"` into the same key, so a
//! project is identified by what it actually points at on disk rather than
//! by how the caller spelled it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Transport-neutral project identity.
///
/// Two contexts are equal when both their `project_id` and `project_root`
/// are byte-equal. The pool canonicalizes `project_root` at storage time,
/// so equality on the stored side always reflects canonical paths.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProjectContext {
    /// Stable project identifier (e.g. `"openab"`, `"ai-workstation"`).
    /// An empty string denotes an anonymous workspace hint — see
    /// [`ProjectContext::anonymous`].
    pub project_id: String,
    /// Absolute or `~`-relative path to the project root. Validated and
    /// canonicalized before storage.
    pub project_root: PathBuf,
}

impl ProjectContext {
    /// Construct an anonymous project context (no `project_id`). Used to
    /// thread the legacy `[[ws:@alias]]` workspace hint through the same
    /// seam as project-pinned sessions without triggering pinning or
    /// mismatch checks.
    pub fn anonymous(project_root: PathBuf) -> Self {
        Self {
            project_id: String::new(),
            project_root,
        }
    }

    /// True when this context has no `project_id` (legacy workspace-only).
    /// Anonymous contexts bypass the per-session project pinning and
    /// mismatch check.
    pub fn is_anonymous(&self) -> bool {
        self.project_id.is_empty()
    }

    /// Validate the project context: `project_root` must be an existing
    /// directory. Returns the canonicalized absolute path on success.
    ///
    /// Canonicalization happens here (not at lookup time) so the stored
    /// binding is stable across equivalent spellings and a mismatch check
    /// does not pay a filesystem syscall on every `get_or_create` call.
    pub fn validate(&self) -> Result<PathBuf, String> {
        if self.project_root.as_os_str().is_empty() {
            return Err("project_root is empty".into());
        }
        let canonical = std::fs::canonicalize(&self.project_root).map_err(|e| {
            format!(
                "project_root {:?} cannot be canonicalized: {}",
                self.project_root, e
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "project_root {:?} is not a directory",
                canonical
            ));
        }
        Ok(canonical)
    }

    /// Validate and return a new `ProjectContext` whose `project_root` is
    /// the canonical absolute path. Idempotent.
    pub fn canonicalized(&self) -> Result<Self, String> {
        let canonical = self.validate()?;
        Ok(Self {
            project_id: self.project_id.clone(),
            project_root: canonical,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn anonymous_context_has_empty_project_id() {
        let p = ProjectContext::anonymous(PathBuf::from("/tmp"));
        assert!(p.is_anonymous());
        assert_eq!(p.project_id, "");
    }

    #[test]
    fn non_anonymous_context_has_project_id() {
        let p = ProjectContext {
            project_id: "openab".into(),
            project_root: PathBuf::from("/tmp"),
        };
        assert!(!p.is_anonymous());
    }

    #[test]
    fn validate_rejects_empty_project_root() {
        let p = ProjectContext::anonymous(PathBuf::new());
        let err = p.validate().expect_err("empty project_root must fail");
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn validate_rejects_nonexistent_root() {
        let p = ProjectContext::anonymous(PathBuf::from(
            "/this/path/does/not/exist/anywhere_2026_08_18",
        ));
        let err = p.validate().expect_err("nonexistent root must fail");
        assert!(
            err.contains("cannot be canonicalized"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_file_root() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("file.txt");
        fs::write(&file_path, b"x").unwrap();

        let p = ProjectContext::anonymous(file_path.clone());
        let err = p.validate().expect_err("file root must fail");
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn validate_accepts_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = ProjectContext::anonymous(dir.path().to_path_buf());
        let canonical = p.validate().expect("existing dir should canonicalize");
        assert_eq!(canonical, fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn canonicalized_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = ProjectContext::anonymous(dir.path().to_path_buf());
        let p2 = p1.canonicalized().expect("canonicalize");
        let p3 = p2.canonicalized().expect("canonicalize is idempotent");
        assert_eq!(p2, p3, "canonicalizing twice yields the same context");
    }

    #[test]
    fn canonicalized_keeps_project_id() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = ProjectContext {
            project_id: "openab".into(),
            project_root: dir.path().to_path_buf(),
        };
        let p2 = p1.canonicalized().expect("canonicalize");
        assert_eq!(p2.project_id, "openab");
        assert!(!p2.is_anonymous());
    }
}

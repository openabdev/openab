//! Control Directives parser (ADR: control-directives.md).
//!
//! Extracts leading `[[key:value]]` directives from the first message in a
//! session, strips them from the prompt, and returns structured metadata.

use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::warn;

static DIRECTIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\[\[([a-z_]+):([^\]]*)\]\]").unwrap());

/// Parsed control directives from a session's first message.
#[derive(Debug, Clone, Default)]
pub struct SessionMetadata {
    /// Resolved canonical workspace path (None = use default working_dir).
    #[allow(dead_code)]
    pub workspace: Option<PathBuf>,
    /// Thread title override (None = use generated title).
    pub title: Option<String>,
    /// Raw directives map for forward-compatible unknown keys.
    pub raw: HashMap<String, String>,
}

/// Result of parsing directives from a prompt.
pub struct ParseResult {
    /// The prompt with leading directives stripped.
    pub prompt: String,
    /// Parsed session metadata.
    pub metadata: SessionMetadata,
}

/// Parse leading `[[key:value]]` directives from a prompt string.
///
/// Directives must appear at the start of the message (after optional
/// whitespace). The first line/token that is not a directive stops parsing;
/// any `[[key:value]]` text after that point is preserved verbatim.
pub fn parse_directives(input: &str) -> ParseResult {
    let mut raw: HashMap<String, String> = HashMap::new();
    let mut remaining = input;

    loop {
        remaining = remaining.trim_start_matches([' ', '\t']);
        if remaining.starts_with('\n') || remaining.starts_with("\r\n") {
            // A blank line after directives = end of header
            let next = remaining.trim_start_matches(['\r', '\n']);
            let next_trimmed = next.trim_start_matches([' ', '\t']);
            if !next_trimmed.starts_with("[[") {
                remaining = next;
                break;
            }
            remaining = remaining.trim_start_matches(['\r', '\n']);
        }
        if let Some(caps) = DIRECTIVE_RE.captures(remaining) {
            let full_match = caps.get(0).unwrap();
            let key = caps[1].to_string();
            let value = caps[2].to_string();
            // Last value wins for duplicate keys
            raw.insert(key, value);
            remaining = &remaining[full_match.end()..];
        } else {
            break;
        }
    }

    let prompt = remaining.trim().to_string();
    let metadata = SessionMetadata {
        workspace: None, // resolved later by resolve_workspace
        title: raw.get("title").cloned(),
        raw,
    };

    ParseResult { prompt, metadata }
}

/// Resolve the `[[ws:...]]` directive value into a canonical path.
///
/// Supports:
/// - Raw paths: `~/projects/foo` or `/home/bot/projects/foo`
/// - Aliases: `@alias_name` → looked up in `aliases` map
///
/// Returns `Err` with a user-visible message on failure.
pub fn resolve_workspace(
    raw_value: &str,
    aliases: &HashMap<String, String>,
    bot_home: &Path,
) -> Result<PathBuf, String> {
    let path_str = if let Some(alias) = raw_value.strip_prefix('@') {
        match aliases.get(alias) {
            Some(resolved) => resolved.as_str(),
            None => {
                let available: Vec<&str> = aliases.keys().map(|s| s.as_str()).collect();
                return Err(format!(
                    "Unknown workspace alias `@{alias}`. Available: {}",
                    if available.is_empty() {
                        "(none configured)".to_string()
                    } else {
                        available.join(", ")
                    }
                ));
            }
        }
    } else {
        raw_value
    };

    // Rule 1: reject relative paths
    if !path_str.starts_with('~') && !path_str.starts_with('/') {
        return Err(format!(
            "Workspace path must be absolute (start with `~` or `/`): `{path_str}`"
        ));
    }

    // Rule 2: expand ~
    let expanded = if let Some(rest) = path_str.strip_prefix('~') {
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        bot_home.join(rest)
    } else {
        PathBuf::from(path_str)
    };

    // Rule 3: canonicalize both paths
    let canonical_home = bot_home.canonicalize().map_err(|e| {
        warn!(path = %bot_home.display(), error = %e, "cannot canonicalize bot home");
        "Internal error: cannot resolve bot home directory".to_string()
    })?;

    let canonical_target = expanded.canonicalize().map_err(|e| {
        warn!(path = %expanded.display(), error = %e, "cannot canonicalize workspace path");
        format!(
            "Workspace path does not exist: `{path_str}` (expanded to `{}`)",
            expanded.display()
        )
    })?;

    // Rule 4+5: verify within bot home subtree
    if !canonical_target.starts_with(&canonical_home) {
        return Err(format!(
            "Workspace path is outside allowed directory: `{path_str}`"
        ));
    }

    // Rule 6: must be a directory (not a file)
    if !canonical_target.is_dir() {
        return Err(format!(
            "Workspace path is not a directory: `{}`",
            canonical_target.display()
        ));
    }

    Ok(canonical_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_basic_directives() {
        let input = "[[ws:~/projects/foo]] [[title:Bug fix]]\ninvestigate the build failure";
        let result = parse_directives(input);
        assert_eq!(result.prompt, "investigate the build failure");
        assert_eq!(result.metadata.raw.get("ws").unwrap(), "~/projects/foo");
        assert_eq!(result.metadata.title.as_deref(), Some("Bug fix"));
    }

    #[test]
    fn parse_directives_multiline_header() {
        let input = "[[ws:@openab]]\n[[title:Review PR]]\nplease review this change";
        let result = parse_directives(input);
        assert_eq!(result.prompt, "please review this change");
        assert_eq!(result.metadata.raw.get("ws").unwrap(), "@openab");
        assert_eq!(result.metadata.title.as_deref(), Some("Review PR"));
    }

    #[test]
    fn parse_preserves_body_directives() {
        let input = "[[title:Test]]\nHere is some code with [[key:value]] in it";
        let result = parse_directives(input);
        assert_eq!(result.prompt, "Here is some code with [[key:value]] in it");
        assert_eq!(result.metadata.title.as_deref(), Some("Test"));
        assert!(!result.metadata.raw.contains_key("key"));
    }

    #[test]
    fn parse_no_directives() {
        let input = "just a regular message";
        let result = parse_directives(input);
        assert_eq!(result.prompt, "just a regular message");
        assert!(result.metadata.raw.is_empty());
    }

    #[test]
    fn parse_duplicate_keys_last_wins() {
        let input = "[[title:First]] [[title:Second]]\ndo stuff";
        let result = parse_directives(input);
        assert_eq!(result.metadata.title.as_deref(), Some("Second"));
    }

    #[test]
    fn parse_empty_value() {
        let input = "[[title:]]\ndo stuff";
        let result = parse_directives(input);
        assert_eq!(result.metadata.title.as_deref(), Some(""));
    }

    #[test]
    fn parse_unknown_keys_ignored() {
        let input = "[[foo:bar]] [[ws:~/x]]\ndo stuff";
        let result = parse_directives(input);
        assert_eq!(result.metadata.raw.get("foo").unwrap(), "bar");
        assert_eq!(result.prompt, "do stuff");
    }

    #[test]
    fn resolve_alias_success() {
        let tmp = TempDir::new().unwrap();
        let projects = tmp.path().join("projects").join("openab");
        fs::create_dir_all(&projects).unwrap();

        let mut aliases = HashMap::new();
        aliases.insert(
            "openab".to_string(),
            format!("{}/projects/openab", tmp.path().display()),
        );

        let result = resolve_workspace("@openab", &aliases, tmp.path()).unwrap();
        assert_eq!(result, projects.canonicalize().unwrap());
    }

    #[test]
    fn resolve_alias_not_found() {
        let tmp = TempDir::new().unwrap();
        let aliases = HashMap::new();
        let result = resolve_workspace("@nope", &aliases, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown workspace alias"));
    }

    #[test]
    fn resolve_relative_path_rejected() {
        let tmp = TempDir::new().unwrap();
        let aliases = HashMap::new();
        let result = resolve_workspace("relative/path", &aliases, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be absolute"));
    }

    #[test]
    fn resolve_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let aliases = HashMap::new();
        let result = resolve_workspace("/tmp", &aliases, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directory"));
    }

    #[test]
    fn resolve_tilde_expansion() {
        let tmp = TempDir::new().unwrap();
        let projects = tmp.path().join("myapp");
        fs::create_dir_all(&projects).unwrap();

        let aliases = HashMap::new();
        let result = resolve_workspace("~/myapp", &aliases, tmp.path()).unwrap();
        assert_eq!(result, projects.canonicalize().unwrap());
    }

    #[test]
    fn resolve_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let aliases = HashMap::new();
        let result = resolve_workspace("~/does_not_exist", &aliases, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn parse_directives_leading_spaces_on_newline() {
        let input = "[[ws:@openab]]\n  [[title:Fix CI]]\nhelp me debug";
        let result = parse_directives(input);
        assert_eq!(result.prompt, "help me debug");
        assert_eq!(result.metadata.raw.get("ws").unwrap(), "@openab");
        assert_eq!(result.metadata.title.as_deref(), Some("Fix CI"));
    }

    #[test]
    fn resolve_file_path_rejected() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("Cargo.toml");
        fs::write(&file_path, "").unwrap();

        let aliases = HashMap::new();
        let result = resolve_workspace(&format!("{}", file_path.display()), &aliases, tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a directory"));
    }

    #[test]
    fn resolve_error_shows_expanded_path() {
        let tmp = TempDir::new().unwrap();
        let aliases = HashMap::new();
        let result = resolve_workspace("~/no_such_dir", &aliases, tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Error should contain both the original and expanded path
        assert!(err.contains("~/no_such_dir"));
        assert!(err.contains(&tmp.path().display().to_string()));
    }
}

//! Parse the untrusted `<role_completion>` block an agent emits in its
//! final reply (workflow `20260818-openab-automatic-three-agent-handoff`).
//!
//! # Trust model
//!
//! The agent may emit arbitrary text. Plain-text forms such as
//! `VERIFIER_PASS`, `PRIMARY_COMPLETE`, `HANDOFF`, or `@ArthurCodex`
//! are **not** completion claims and must not be treated as such.
//! Only an exact `<role_completion>…</role_completion>` block counts.
//!
//! A parsed [`ParsedClaim`] is still UNTRUSTED — every field on it is
//! raw agent output. The trusted validator
//! ([`super::validator`]) runs the 10-check rule list against the
//! project-local [`super::assignment::WorkflowAssignment`] before any
//! transition is permitted to commit.
//!
//! # Multiple blocks
//!
//! The parser does **not** use "first block wins". If the agent emits
//! more than one `<role_completion>` block in the same final reply the
//! whole turn is rejected as [`ParseOutcome::AmbiguousMultipleClaims`]
//! so the workflow engine never has to guess which claim the agent
//! intended.
//!
//! # Fenced output
//!
//! The parser accepts both raw blocks and blocks wrapped in a
//! ` ```text ` fenced code region. The fence is irrelevant to parsing
//! — the inner `<role_completion>` markers are what matter.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use super::state::{CompletionResult, WorkflowRole};

/// Opening marker for a completion block.
const OPENING_MARKER: &str = "<role_completion>";

/// Closing marker for a completion block.
const CLOSING_MARKER: &str = "</role_completion>";

/// Fields that MUST be present in every well-formed block.
const REQUIRED_FIELDS: &[&str] = &[
    "role",
    "result",
    "workflow_id",
    "project_id",
    "project_root",
];

/// Fields that MAY be present. Anything else fails closed.
const OPTIONAL_FIELDS: &[&str] = &["scope", "timestamp"];

/// Fields that MUST NEVER be present. The agent is forbidden from
/// supplying any of these — they are owned by OpenAB's trusted state.
const FORBIDDEN_FIELDS: &[&str] = &[
    "workflow_revision",
    "transition_id",
    "next_role",
    "next_stage",
    "target_user_id",
];

/// Regex matching one `<role_completion>…</role_completion>` block
/// (non-greedy). Used with `captures_iter` so we can count every
/// block in the input rather than just the first one.
///
/// The markers are sourced from [`OPENING_MARKER`] and
/// [`CLOSING_MARKER`] so the regex and the constants cannot drift.
fn block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let pattern = format!(r"(?s){}(.*?){}", OPENING_MARKER, CLOSING_MARKER);
        Regex::new(&pattern).unwrap()
    })
}

/// One well-formed completion claim parsed from a single block. Every
/// field is UNTRUSTED until the validator passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedClaim {
    pub role: WorkflowRole,
    pub result: CompletionResult,
    pub workflow_id: String,
    pub project_id: String,
    pub project_root: PathBuf,
    pub scope: Option<String>,
    pub timestamp: Option<String>,
}

/// What `parse_role_completion` actually returns. The validator only
/// proceeds when the outcome is [`ParseOutcome::ParsedClaim`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// No `<role_completion>` block was present. Plain text like
    /// `VERIFIER_PASS` / `HANDOFF` / `@ArthurCodex` lands here.
    NoClaim,

    /// Exactly one well-formed block was present. Trust is still
    /// untrusted — run the validator next.
    ParsedClaim(ParsedClaim),

    /// More than one `<role_completion>` block was present. The
    /// workflow engine must not guess which claim the agent intended,
    /// so the whole turn is rejected.
    AmbiguousMultipleClaims,

    /// A single block was found but it was malformed (missing
    /// required field, forbidden field, unknown field, invalid
    /// `role` / `result` value, etc.). The `reason` string is a
    /// stable diagnostic token suitable for audit logging.
    MalformedBlock { reason: String },
}

/// Parse an assistant reply for completion blocks.
///
/// Behaviour:
/// - **Zero** blocks → [`ParseOutcome::NoClaim`].
/// - **Exactly one** well-formed block → [`ParseOutcome::ParsedClaim`].
/// - **Exactly one** malformed block → [`ParseOutcome::MalformedBlock`]
///   with the first diagnostic reason.
/// - **More than one** block (regardless of well-formed-ness) →
///   [`ParseOutcome::AmbiguousMultipleClaims`]. The whole turn is
///   rejected.
///
/// Plain text outside the markers is ignored.
pub fn parse_role_completion(text: &str) -> ParseOutcome {
    let captures: Vec<&str> = block_re()
        .captures_iter(text)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();

    match captures.len() {
        0 => ParseOutcome::NoClaim,
        1 => parse_single_block(captures[0]),
        _ => ParseOutcome::AmbiguousMultipleClaims,
    }
}

fn parse_single_block(body: &str) -> ParseOutcome {
    let fields = match parse_block_fields(body) {
        Ok(f) => f,
        Err(reason) => return ParseOutcome::MalformedBlock { reason },
    };

    // Forbidden fields: fail closed even if required fields are also
    // present. The agent must not author any of these — supplying one
    // is a structural violation regardless of what else it sent.
    for &f in FORBIDDEN_FIELDS {
        if fields.contains_key(f) {
            return ParseOutcome::MalformedBlock {
                reason: format!("forbidden field {f:?}"),
            };
        }
    }

    // Required fields: every one must be present and non-empty.
    for &f in REQUIRED_FIELDS {
        match fields.get(f) {
            Some(v) if !v.is_empty() => {}
            Some(_) => {
                return ParseOutcome::MalformedBlock {
                    reason: format!("required field {f:?} is empty"),
                };
            }
            None => {
                return ParseOutcome::MalformedBlock {
                    reason: format!("missing required field {f:?}"),
                };
            }
        }
    }

    // Optional fields: present-or-absent only. Anything else fails
    // closed.
    for key in fields.keys() {
        if REQUIRED_FIELDS.iter().all(|r| *r != key) && OPTIONAL_FIELDS.iter().all(|o| *o != key) {
            return ParseOutcome::MalformedBlock {
                reason: format!("unknown field {key:?}"),
            };
        }
    }

    // Role / result must be one of the canonical enum spellings.
    let role_str = fields.get("role").unwrap();
    let role = match WorkflowRole::from_canonical(role_str) {
        Some(r) => r,
        None => {
            return ParseOutcome::MalformedBlock {
                reason: format!("invalid role {role_str:?}"),
            }
        }
    };
    let result_str = fields.get("result").unwrap();
    let result = match CompletionResult::from_canonical(result_str) {
        Some(r) => r,
        None => {
            return ParseOutcome::MalformedBlock {
                reason: format!("invalid result {result_str:?}"),
            }
        }
    };

    ParseOutcome::ParsedClaim(ParsedClaim {
        role,
        result,
        workflow_id: fields.get("workflow_id").unwrap().clone(),
        project_id: fields.get("project_id").unwrap().clone(),
        project_root: PathBuf::from(fields.get("project_root").unwrap()),
        scope: fields.get("scope").cloned(),
        timestamp: fields.get("timestamp").cloned(),
    })
}

/// Parse `key: value` lines into a `HashMap`. Empty / whitespace-only
/// lines are skipped. Duplicate keys fail closed. A line without a
/// `:` separator fails closed.
fn parse_block_fields(body: &str) -> Result<HashMap<String, String>, String> {
    let mut fields: HashMap<String, String> = HashMap::new();
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid line {line:?}"))?;
        let key = key.trim().to_lowercase();
        let value = value.trim();
        if key.is_empty() {
            return Err(format!("empty key in line {raw_line:?}"));
        }
        if fields.insert(key.clone(), value.to_string()).is_some() {
            return Err(format!("duplicate key {key:?}"));
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn well_formed_block() -> &'static str {
        "<role_completion>\n\
         role: PRIMARY\n\
         result: COMPLETE\n\
         workflow_id: wf-001\n\
         project_id: openab\n\
         project_root: /tmp/openab\n\
         scope: review current diff\n\
         timestamp: 2026-08-18T00:00:00Z\n\
         </role_completion>"
    }

    #[test]
    fn well_formed_raw_block_parses() {
        let outcome = parse_role_completion(well_formed_block());
        match outcome {
            ParseOutcome::ParsedClaim(c) => {
                assert_eq!(c.role, WorkflowRole::Primary);
                assert_eq!(c.result, CompletionResult::Complete);
                assert_eq!(c.workflow_id, "wf-001");
                assert_eq!(c.project_id, "openab");
                assert_eq!(c.project_root, PathBuf::from("/tmp/openab"));
                assert_eq!(c.scope.as_deref(), Some("review current diff"));
                assert_eq!(c.timestamp.as_deref(), Some("2026-08-18T00:00:00Z"));
            }
            other => panic!("expected ParsedClaim, got {other:?}"),
        }
    }

    #[test]
    fn well_formed_fenced_block_parses() {
        let text = "Here is my final report.\n\n\
                    ```text\n\
                    <role_completion>\n\
                    role: VERIFIER\n\
                    result: PASS\n\
                    workflow_id: wf-002\n\
                    project_id: openab\n\
                    project_root: /tmp/openab\n\
                    </role_completion>\n\
                    ```\n";
        let outcome = parse_role_completion(text);
        match outcome {
            ParseOutcome::ParsedClaim(c) => {
                assert_eq!(c.role, WorkflowRole::Verifier);
                assert_eq!(c.result, CompletionResult::Pass);
                assert_eq!(c.workflow_id, "wf-002");
                assert!(c.scope.is_none());
                assert!(c.timestamp.is_none());
            }
            other => panic!("expected ParsedClaim, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_field_is_malformed() {
        let text = "<role_completion>\n\
                    role: PRIMARY\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("project_root"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn empty_required_field_is_malformed() {
        let text = "<role_completion>\n\
                    role:\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(
                    reason.contains("role") && reason.contains("empty"),
                    "reason was {reason:?}"
                );
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn forbidden_field_is_rejected() {
        for forbidden in FORBIDDEN_FIELDS {
            let text = format!(
                "<role_completion>\n\
                 role: PRIMARY\n\
                 result: COMPLETE\n\
                 workflow_id: wf-001\n\
                 project_id: openab\n\
                 project_root: /tmp\n\
                 {forbidden}: sneaky\n\
                 </role_completion>"
            );
            match parse_role_completion(&text) {
                ParseOutcome::MalformedBlock { reason } => {
                    assert!(
                        reason.contains("forbidden"),
                        "expected forbidden-field reason for {forbidden:?}, got {reason:?}"
                    );
                }
                other => panic!("expected MalformedBlock for {forbidden:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let text = "<role_completion>\n\
                    role: PRIMARY\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    surprise: yes\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("unknown"), "reason was {reason:?}");
                assert!(reason.contains("surprise"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn invalid_role_value_is_malformed() {
        let text = "<role_completion>\n\
                    role: BOSS\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("invalid role"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn invalid_result_value_is_malformed() {
        let text = "<role_completion>\n\
                    role: PRIMARY\n\
                    result: MAYBE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("invalid result"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn zero_blocks_is_no_claim() {
        assert_eq!(parse_role_completion(""), ParseOutcome::NoClaim);
        assert_eq!(
            parse_role_completion("plain narrative text, no claim here"),
            ParseOutcome::NoClaim
        );
    }

    #[test]
    fn plain_text_mentions_do_not_count() {
        // The agent might sprinkle trigger-looking tokens in prose.
        // None of these is a completion claim.
        let samples = [
            "VERIFIER_PASS",
            "PRIMARY_COMPLETE",
            "HANDOFF",
            "@ArthurCodex",
            "@ArthurGemini",
            "<role_completion> with no closing",
            "HANDOFF\nto: @ArthurGemini",
        ];
        for s in samples {
            assert_eq!(
                parse_role_completion(s),
                ParseOutcome::NoClaim,
                "plain-text sample wrongly recognized as claim: {s:?}"
            );
        }
    }

    #[test]
    fn multiple_blocks_is_ambiguous() {
        let text = "<role_completion>\n\
                    role: PRIMARY\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>\n\n\
                    and also:\n\n\
                    <role_completion>\n\
                    role: VERIFIER\n\
                    result: PASS\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        assert_eq!(
            parse_role_completion(text),
            ParseOutcome::AmbiguousMultipleClaims
        );
    }

    #[test]
    fn duplicate_key_is_malformed() {
        let text = "<role_completion>\n\
                    role: PRIMARY\n\
                    role: SECONDARY\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("duplicate"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn line_without_colon_is_malformed() {
        let text = "<role_completion>\n\
                    role PRIMARY\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("invalid line"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn surrounding_text_is_ignored() {
        let text = "Some prose.\n\
                    \n\
                    <role_completion>\n\
                    role: PRIMARY\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>\n\
                    \n\
                    More prose after.";
        let outcome = parse_role_completion(text);
        match outcome {
            ParseOutcome::ParsedClaim(c) => {
                assert_eq!(c.role, WorkflowRole::Primary);
            }
            other => panic!("expected ParsedClaim, got {other:?}"),
        }
    }

    #[test]
    fn keys_are_case_insensitive() {
        // Keys are lowercased internally; mixed-case keys still match
        // the canonical lookup. Values for `role` and `result` must
        // remain in the canonical SCREAMING_SNAKE_CASE form.
        let text = "<role_completion>\n\
                    Role: PRIMARY\n\
                    RESULT: COMPLETE\n\
                    workflow_id: wf-001\n\
                    PROJECT_ID: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        let outcome = parse_role_completion(text);
        match outcome {
            ParseOutcome::ParsedClaim(c) => {
                assert_eq!(c.role, WorkflowRole::Primary);
                assert_eq!(c.result, CompletionResult::Complete);
            }
            other => panic!("expected ParsedClaim, got {other:?}"),
        }
    }

    #[test]
    fn non_canonical_value_for_role_is_rejected() {
        // Keys are case-insensitive, but values must be canonical.
        let text = "<role_completion>\n\
                    role: primary\n\
                    result: COMPLETE\n\
                    workflow_id: wf-001\n\
                    project_id: openab\n\
                    project_root: /tmp\n\
                    </role_completion>";
        match parse_role_completion(text) {
            ParseOutcome::MalformedBlock { reason } => {
                assert!(reason.contains("invalid role"), "reason was {reason:?}");
            }
            other => panic!("expected MalformedBlock, got {other:?}"),
        }
    }

    #[test]
    fn opening_marker_exposed_for_diagnostics() {
        // Sanity: the marker constants stay in sync with the regex.
        assert_eq!(OPENING_MARKER, "<role_completion>");
        assert_eq!(CLOSING_MARKER, "</role_completion>");
    }
}

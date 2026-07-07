//! Schema definition + validation for `docs/platforms/schema/*.toml`.
//!
//! The blank template and human-readable field docs live in
//! `docs/platforms/_template.toml`; this crate is the machine-enforced side.
//! [`validate`] checks one parsed file against the schema (required fields,
//! closed enum sets, the closed feature set, unknown-key rejection), and
//! [`feature_code_refs`] / [`quirk_code_refs`] surface the `source` code-refs so
//! the conformance tests can prove they still exist in the tree.
//!
//! Parsing uses `toml_edit` in parse-only mode: no proc-macros, no build
//! scripts, so the whole crate compiles and tests with no C toolchain.

use toml_edit::{DocumentMut, Item, Table};

/// Current schema version. Bump when the schema changes; stale files are flagged.
pub const SCHEMA_VERSION: &str = "2026-07-07";

/// The complete, closed OpenAB feature set (Schema 2). Every platform file must
/// contain exactly these keys, once each.
pub const EXPECTED_FEATURES: &[&str] = &[
    "send_message",
    "message_split",
    "streaming",
    "reply_quote",
    "edit_message",
    "delete_message",
    "emoji_reactions",
    "threads_topics",
    "media_inbound",
    "voice_stt",
    "trust_gate",
    "deny_echo",
    "mention_gating",
    "slash_commands",
    "multibot",
    "group_routing",
];

/// The allowed `status` values for an OpenAB feature.
pub const FEATURE_STATUS: &[&str] = &[
    "implemented",
    "partial",
    "workaround",
    "not_implemented",
    "n_a",
];

/// Statuses that claim the feature is present, so they must cite a source.
const STATUS_NEEDS_SOURCE: &[&str] = &["implemented", "partial", "workaround"];

// ─── capability section specs ───────────────────────────────────────────────
// Each field: (name, kind). Kind drives the type/enum check. Every capability
// section also implicitly requires `note` (string) + `source` (string), added
// automatically, so specs below list only the section-specific fields.

enum Kind {
    Bool,
    Uint,
    OptUint,
    StrArray,
    /// String value constrained to a closed set.
    Enum(&'static [&'static str]),
    /// Array whose every element is constrained to a closed set.
    EnumArray(&'static [&'static str]),
}

struct Spec {
    section: &'static str,
    fields: &'static [(&'static str, Kind)],
}

const TRANSPORT: &[&str] = &["webhook", "websocket", "socket_mode", "long_poll"];
const AUTH: &[&str] = &[
    "hmac_sha256",
    "jwt_rs256",
    "aes",
    "shared_secret",
    "oauth",
    "none",
];
const THREADS: &[&str] = &["native", "reply_to_only", "emulated", "none"];
const MENTION: &[&str] = &["at_mention", "username", "self_flag", "none"];
const DELETE_SCOPE: &[&str] = &["none", "own", "others", "own_and_others"];
const ATTACH: &[&str] = &["image", "audio", "video", "file"];
const STABLE_ID: &[&str] = &["yes", "no", "consent_gated"];
const SEND_MODEL: &[&str] = &["any_time", "reply_only", "push_only", "hybrid"];
const QUOTA: &[&str] = &["unlimited", "metered", "none"];

/// Every `[capability.*]` sub-section in template order.
pub const CAPABILITY_SECTIONS: &[&str] = &[
    "transport",
    "inbound_auth",
    "threads",
    "slash_commands",
    "mentions",
    "emoji_reactions",
    "edit_message",
    "delete_message",
    "rich_content",
    "attachments",
    "message_length_limit",
    "dm_support",
    "group_model",
    "group_sender_identity",
    "send_model",
    "proactive_push",
    "bot_to_bot",
    "typing_indicator",
];

fn capability_specs() -> &'static [Spec] {
    use Kind::*;
    &[
        Spec { section: "transport", fields: &[("kind", Enum(TRANSPORT))] },
        Spec { section: "inbound_auth", fields: &[("scheme", Enum(AUTH))] },
        Spec { section: "threads", fields: &[("model", Enum(THREADS))] },
        Spec { section: "slash_commands", fields: &[("supported", Bool)] },
        Spec { section: "mentions", fields: &[("method", Enum(MENTION))] },
        Spec {
            section: "emoji_reactions",
            fields: &[
                ("bot_can_add", Bool),
                ("bot_can_remove", Bool),
                ("bot_receives_events", Bool),
            ],
        },
        Spec { section: "edit_message", fields: &[("supported", Bool)] },
        Spec {
            section: "delete_message",
            fields: &[("supported", Bool), ("scope", Enum(DELETE_SCOPE))],
        },
        Spec {
            section: "rich_content",
            fields: &[("markdown", Bool), ("cards", Bool), ("buttons", Bool)],
        },
        Spec {
            section: "attachments",
            fields: &[
                ("inbound", EnumArray(ATTACH)),
                ("outbound", EnumArray(ATTACH)),
                ("max_size_mb", OptUint),
            ],
        },
        Spec { section: "message_length_limit", fields: &[("max_chars", Uint)] },
        Spec { section: "dm_support", fields: &[("supported", Bool)] },
        Spec { section: "group_model", fields: &[("kinds", StrArray)] },
        Spec { section: "group_sender_identity", fields: &[("stable_id", Enum(STABLE_ID))] },
        Spec {
            section: "send_model",
            fields: &[
                ("model", Enum(SEND_MODEL)),
                ("reply_token_ttl_sec", OptUint),
                ("max_objects_per_send", OptUint),
            ],
        },
        Spec {
            section: "proactive_push",
            fields: &[("supported", Bool), ("quota_model", Enum(QUOTA))],
        },
        Spec { section: "bot_to_bot", fields: &[("delivered", Bool)] },
        Spec { section: "typing_indicator", fields: &[("supported", Bool)] },
    ]
}

// ─── validation ─────────────────────────────────────────────────────────────

/// Validate one parsed schema file (named `name`, e.g. "line") against the
/// schema. Returns a list of human-readable errors; empty means conforming.
pub fn validate(doc: &DocumentMut, name: &str) -> Vec<String> {
    let mut e = Vec::new();
    let root = doc.as_table();

    // top-level: schema_version + platform + capability + arrays
    check_unknown_keys(
        root,
        &["schema_version", "platform", "capability", "openab_features", "quirks"],
        "(top level)",
        &mut e,
    );

    match root.get("schema_version").and_then(Item::as_str) {
        Some(v) if v == SCHEMA_VERSION => {}
        Some(v) => e.push(format!("schema_version is {v:?}, expected {SCHEMA_VERSION:?} (stale)")),
        None => e.push("missing schema_version (string)".into()),
    }

    validate_platform(root, name, &mut e);
    validate_capability(root, &mut e);
    validate_features(root, &mut e);
    validate_quirks(root, &mut e);
    e
}

fn validate_platform(root: &Table, name: &str, e: &mut Vec<String>) {
    let Some(t) = root.get("platform").and_then(Item::as_table) else {
        e.push("missing [platform] table".into());
        return;
    };
    check_unknown_keys(t, &["name", "official_docs", "description"], "[platform]", e);
    req_str(t, "name", "[platform]", e);
    req_str(t, "official_docs", "[platform]", e);
    req_str(t, "description", "[platform]", e);
    if let Some(n) = t.get("name").and_then(Item::as_str) {
        if n != name {
            e.push(format!("[platform].name is {n:?} but must match filename {name:?}"));
        }
    }
}

fn validate_capability(root: &Table, e: &mut Vec<String>) {
    let Some(cap) = root.get("capability").and_then(Item::as_table) else {
        e.push("missing [capability] table".into());
        return;
    };
    let known: Vec<&str> = CAPABILITY_SECTIONS.to_vec();
    check_unknown_keys(cap, &known, "[capability]", e);

    for spec in capability_specs() {
        let ctx = format!("[capability.{}]", spec.section);
        let Some(t) = cap.get(spec.section).and_then(Item::as_table) else {
            e.push(format!("missing {ctx}"));
            continue;
        };
        // allowed keys = spec fields + note + source
        let mut allowed: Vec<&str> = spec.fields.iter().map(|(n, _)| *n).collect();
        allowed.push("note");
        allowed.push("source");
        check_unknown_keys(t, &allowed, &ctx, e);

        for (field, kind) in spec.fields {
            check_field(t, field, kind, &ctx, e);
        }
        req_str(t, "note", &ctx, e);
        req_str(t, "source", &ctx, e);
    }
}

fn validate_features(root: &Table, e: &mut Vec<String>) {
    let Some(arr) = root.get("openab_features").and_then(Item::as_array_of_tables) else {
        e.push("missing [[openab_features]] (must have all 16)".into());
        return;
    };
    let mut seen: Vec<String> = Vec::new();
    for t in arr.iter() {
        let ctx = "[[openab_features]]";
        check_unknown_keys(t, &["feature", "status", "note", "source", "pr"], ctx, e);
        let feat = t.get("feature").and_then(Item::as_str);
        match feat {
            Some(f) if EXPECTED_FEATURES.contains(&f) => {
                if seen.iter().any(|s| s == f) {
                    e.push(format!("duplicate feature {f:?}"));
                }
                seen.push(f.to_string());
            }
            Some(f) => e.push(format!("unknown feature key {f:?}")),
            None => e.push("feature block missing `feature` (string)".into()),
        }
        let fctx = feat.map(|f| format!("feature {f:?}")).unwrap_or_else(|| ctx.into());

        let status = t.get("status").and_then(Item::as_str);
        match status {
            Some(s) if FEATURE_STATUS.contains(&s) => {}
            Some(s) => e.push(format!("{fctx}: invalid status {s:?}")),
            None => e.push(format!("{fctx}: missing status")),
        }
        req_str(t, "note", &fctx, e);
        // source: array of strings (code refs)
        let srcs = str_array(t, "source");
        if srcs.is_none() {
            e.push(format!("{fctx}: `source` must be an array of strings"));
        }
        if let (Some(s), Some(list)) = (status, &srcs) {
            if STATUS_NEEDS_SOURCE.contains(&s) && list.is_empty() {
                e.push(format!("{fctx}: status {s:?} must cite at least one source"));
            }
        }
        // pr optional string
        if let Some(pr) = t.get("pr") {
            if !pr.is_none() && pr.as_str().is_none() {
                e.push(format!("{fctx}: `pr` must be a string"));
            }
        }
    }
    // closed-set completeness
    for want in EXPECTED_FEATURES {
        if !seen.iter().any(|s| s == want) {
            e.push(format!("missing feature block {want:?}"));
        }
    }
}

fn validate_quirks(root: &Table, e: &mut Vec<String>) {
    // quirks optional as a whole, but if present each block is validated.
    let Some(item) = root.get("quirks") else { return };
    let Some(arr) = item.as_array_of_tables() else {
        e.push("[[quirks]] must be an array of tables".into());
        return;
    };
    for t in arr.iter() {
        let title = t.get("title").and_then(Item::as_str).unwrap_or("<untitled>");
        let ctx = format!("quirk {title:?}");
        check_unknown_keys(t, &["date", "title", "note", "kind", "source", "refs"], &ctx, e);
        req_str(t, "date", &ctx, e);
        req_str(t, "title", &ctx, e);
        req_str(t, "note", &ctx, e);
        match t.get("kind").and_then(Item::as_str) {
            Some(k) if k == "intrinsic" || k == "openab_decision" => {}
            Some(k) => e.push(format!("{ctx}: invalid kind {k:?} (intrinsic|openab_decision)")),
            None => e.push(format!("{ctx}: missing kind")),
        }
        if let Some(src) = t.get("source") {
            if !src.is_none() && src.as_str().is_none() {
                e.push(format!("{ctx}: `source` must be a string"));
            }
        }
        if let Some(r) = t.get("refs") {
            if !r.is_none() && str_array(t, "refs").is_none() {
                e.push(format!("{ctx}: `refs` must be an array of strings"));
            }
        }
    }
}

// ─── code-ref extraction (for the existence tests) ──────────────────────────

/// (context, ref) for every code-ref in `[[openab_features]].source`.
pub fn feature_code_refs(doc: &DocumentMut) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(arr) = doc.as_table().get("openab_features").and_then(Item::as_array_of_tables) {
        for t in arr.iter() {
            let feat = t.get("feature").and_then(Item::as_str).unwrap_or("?");
            for s in str_array(t, "source").unwrap_or_default() {
                out.push((format!("feature {feat:?}"), s));
            }
        }
    }
    out
}

/// (context, ref) for every code-ref in a quirk `source` (URLs skipped).
pub fn quirk_code_refs(doc: &DocumentMut) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(arr) = doc.as_table().get("quirks").and_then(Item::as_array_of_tables) {
        for t in arr.iter() {
            let title = t.get("title").and_then(Item::as_str).unwrap_or("?");
            if let Some(s) = t.get("source").and_then(Item::as_str) {
                if is_code_ref(s) {
                    out.push((format!("quirk {title:?}"), s.to_string()));
                }
            }
        }
    }
    out
}

/// A parsed code-ref source: `"path/to/file.rs"` or `"path/to/file.rs#symbol"`.
pub struct CodeRef<'a> {
    pub file: &'a str,
    pub symbol: Option<&'a str>,
}

pub fn parse_code_ref(s: &str) -> CodeRef<'_> {
    match s.split_once('#') {
        Some((file, symbol)) => CodeRef { file, symbol: Some(symbol) },
        None => CodeRef { file: s, symbol: None },
    }
}

/// Does this source look like an in-repo code ref (vs an official-doc URL)?
pub fn is_code_ref(s: &str) -> bool {
    !s.starts_with("http://") && !s.starts_with("https://")
}

// ─── small typed helpers ────────────────────────────────────────────────────

fn check_field(t: &Table, field: &str, kind: &Kind, ctx: &str, e: &mut Vec<String>) {
    match kind {
        Kind::Bool => {
            if t.get(field).and_then(Item::as_bool).is_none() {
                e.push(format!("{ctx}: `{field}` must be a bool"));
            }
        }
        Kind::Uint => match t.get(field).and_then(Item::as_integer) {
            Some(n) if n >= 0 => {}
            _ => e.push(format!("{ctx}: `{field}` must be a non-negative integer")),
        },
        Kind::OptUint => {
            if let Some(item) = t.get(field) {
                if !item.is_none() {
                    match item.as_integer() {
                        Some(n) if n >= 0 => {}
                        _ => e.push(format!("{ctx}: `{field}` must be a non-negative integer")),
                    }
                }
            }
        }
        Kind::StrArray => {
            if str_array(t, field).is_none() {
                e.push(format!("{ctx}: `{field}` must be an array of strings"));
            }
        }
        Kind::Enum(allowed) => match t.get(field).and_then(Item::as_str) {
            Some(v) if allowed.contains(&v) => {}
            Some(v) => e.push(format!("{ctx}: `{field}` = {v:?} not in {allowed:?}")),
            None => e.push(format!("{ctx}: `{field}` must be one of {allowed:?}")),
        },
        Kind::EnumArray(allowed) => match str_array(t, field) {
            None => e.push(format!("{ctx}: `{field}` must be an array of strings")),
            Some(list) => {
                for v in list {
                    if !allowed.contains(&v.as_str()) {
                        e.push(format!("{ctx}: `{field}` element {v:?} not in {allowed:?}"));
                    }
                }
            }
        },
    }
}

fn req_str(t: &Table, key: &str, ctx: &str, e: &mut Vec<String>) {
    if t.get(key).and_then(Item::as_str).is_none() {
        e.push(format!("{ctx}: `{key}` must be a string"));
    }
}

fn str_array(t: &Table, key: &str) -> Option<Vec<String>> {
    let arr = t.get(key)?.as_array()?;
    let mut out = Vec::new();
    for v in arr.iter() {
        out.push(v.as_str()?.to_string());
    }
    Some(out)
}

fn check_unknown_keys(t: &Table, allowed: &[&str], ctx: &str, e: &mut Vec<String>) {
    for (k, _) in t.iter() {
        if !allowed.contains(&k) {
            e.push(format!("{ctx}: unknown key `{k}`"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_ref_parsing() {
        let r = parse_code_ref("crates/a/src/b.rs#foo");
        assert_eq!(r.file, "crates/a/src/b.rs");
        assert_eq!(r.symbol, Some("foo"));
        let r = parse_code_ref("crates/a/src/b.rs");
        assert_eq!(r.symbol, None);
        assert!(is_code_ref("crates/a.rs"));
        assert!(!is_code_ref("https://example.com"));
    }

    #[test]
    fn validate_flags_a_broken_file() {
        // An empty doc should trip many required-field errors — proves the
        // checker isn't vacuously passing.
        let doc: DocumentMut = "schema_version = \"1999-01-01\"".parse().unwrap();
        let errs = validate(&doc, "line");
        assert!(!errs.is_empty());
        assert!(errs.iter().any(|e| e.contains("stale")), "should flag stale version");
        assert!(errs.iter().any(|e| e.contains("[platform]")), "should flag missing platform");
        assert!(errs.iter().any(|e| e.contains("openab_features")), "should flag missing features");
    }

    #[test]
    fn validate_flags_a_bad_enum() {
        let doc: DocumentMut = "[capability.transport]\nkind = \"carrier_pigeon\"\nnote = \"x\"\nsource = \"y\""
            .parse()
            .unwrap();
        let errs = validate(&doc, "line");
        assert!(
            errs.iter().any(|e| e.contains("carrier_pigeon")),
            "should reject an out-of-set enum value"
        );
    }
}

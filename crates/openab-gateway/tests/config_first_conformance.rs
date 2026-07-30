//! Config-first conformance guard (#1375).
//!
//! Invariant: **every platform environment variable referenced anywhere in
//! the workspace (this crate, `openab-core`, and the `openab` binary) must
//! have a corresponding first-class `[section]` field in `openab-core`'s
//! config**, so that the resolution order
//! `config → PLATFORM_* env → default` holds for every setting (config always
//! wins; env is a fallback only — never the sole source).
//!
//! This test scans the crate's own sources for platform-prefixed env-var
//! string literals and asserts each one is in the canonical covered set
//! below. It exists to prevent regression to env-first: adding a new
//! `FEISHU_NEW_KNOB` (or similar) read without a matching section field
//! fails this test with instructions.
//!
//! Deterministic source-scan over the crate's own files (same pattern as the
//! `platform-schema` conformance suite) — no network, no external state.

use std::collections::BTreeSet;
use std::path::Path;

/// The canonical covered set: every platform env var that has a matching
/// first-class section field in `openab-core::config` (see each struct's
/// per-field env fallbacks):
///
/// - `[telegram]` → `TelegramConfig` (#1297)
/// - `[line]`     → `LineConfig` (#1381)
/// - `[wecom]`    → `WecomConfig` (#1382)
/// - `[googlechat]` → `GoogleChatConfig` (#1383)
/// - `[teams]`    → `TeamsConfig` (#1384)
/// - `[feishu]`   → `FeishuConfig` (#1385)
/// - `[lineworks]` → `LineWorksConfig`
///
/// When you add a platform env var to this crate:
/// 1. add the matching field to the platform's section struct in
///    `crates/openab-core/src/config.rs` (config-first, env fallback),
/// 2. thread it through the platform's `Gateway*Config` bridge / `apply_*`,
/// 3. document it in `docs/config-reference.md`, and
/// 4. add it here.
const COVERED: &[&str] = &[
    // telegram
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_SECRET_TOKEN",
    "TELEGRAM_TRUSTED_SOURCE_ONLY",
    "TELEGRAM_RICH_MESSAGES",
    "TELEGRAM_STREAMING",
    "TELEGRAM_WEBHOOK_PATH",
    "TELEGRAM_ALLOW_ALL_USERS",
    "TELEGRAM_ALLOWED_USERS",
    // line
    "LINE_CHANNEL_SECRET",
    "LINE_CHANNEL_ACCESS_TOKEN",
    "LINE_WEBHOOK_PATH",
    "LINE_ALLOW_ALL_USERS",
    "LINE_ALLOWED_USERS",
    // feishu
    "FEISHU_APP_ID",
    "FEISHU_APP_SECRET",
    "FEISHU_VERIFICATION_TOKEN",
    "FEISHU_ENCRYPT_KEY",
    "FEISHU_DOMAIN",
    "FEISHU_CONNECTION_MODE",
    "FEISHU_WEBHOOK_PATH",
    "FEISHU_ALLOWED_GROUPS",
    "FEISHU_ALLOWED_USERS",
    "FEISHU_REQUIRE_MENTION",
    "FEISHU_ALLOW_BOTS",
    "FEISHU_ALLOW_USER_MESSAGES",
    "FEISHU_TRUSTED_BOT_IDS",
    "FEISHU_MAX_BOT_TURNS",
    "FEISHU_DEDUPE_TTL_SECS",
    "FEISHU_MESSAGE_LIMIT",
    "FEISHU_SESSION_TTL_HOURS",
    "FEISHU_CARD_STREAMING_MODE",
    "FEISHU_CARD_FALLBACK_TO_POST",
    "FEISHU_CARD_PROMOTE_BYTES",
    "FEISHU_CARD_IDLE_FINALIZE_MS",
    "FEISHU_ALLOW_ALL_USERS",
    // wecom
    "WECOM_CORP_ID",
    "WECOM_SECRET",
    "WECOM_TOKEN",
    "WECOM_ENCODING_AES_KEY",
    "WECOM_AGENT_ID",
    "WECOM_WEBHOOK_PATH",
    "WECOM_STREAMING_ENABLED",
    "WECOM_DEBOUNCE_SECS",
    "WECOM_ALLOW_ALL_USERS",
    "WECOM_ALLOWED_USERS",
    // googlechat
    "GOOGLE_CHAT_ENABLED",
    "GOOGLE_CHAT_SA_KEY_JSON",
    "GOOGLE_CHAT_SA_KEY_FILE",
    "GOOGLE_CHAT_ACCESS_TOKEN",
    "GOOGLE_CHAT_AUDIENCE",
    "GOOGLE_CHAT_WEBHOOK_PATH",
    "GOOGLE_CHAT_ALLOW_ALL_USERS",
    "GOOGLE_CHAT_ALLOWED_USERS",
    // teams
    "TEAMS_APP_ID",
    "TEAMS_APP_SECRET",
    "TEAMS_ALLOWED_TENANTS",
    "TEAMS_OAUTH_ENDPOINT",
    "TEAMS_OPENID_METADATA",
    "TEAMS_WEBHOOK_PATH",
    "TEAMS_ALLOW_ALL_USERS",
    "TEAMS_ALLOWED_USERS",
    // lineworks
    "LINEWORKS_BOT_ID",
    "LINEWORKS_BOT_SECRET",
    "LINEWORKS_CLIENT_ID",
    "LINEWORKS_CLIENT_SECRET",
    "LINEWORKS_SERVICE_ACCOUNT",
    "LINEWORKS_PRIVATE_KEY",
    "LINEWORKS_PRIVATE_KEY_FILE",
    "LINEWORKS_WEBHOOK_PATH",
    "LINEWORKS_REQUIRE_MENTION",
    "LINEWORKS_BOT_NAME",
    "LINEWORKS_RICH_MESSAGES",
    "LINEWORKS_ACK_MESSAGE",
    "LINEWORKS_ALLOW_ALL_USERS",
    "LINEWORKS_ALLOWED_USERS",
];

const PLATFORM_PREFIXES: &[&str] = &[
    "TELEGRAM_",
    "LINE_",
    "LINEWORKS_",
    "FEISHU_",
    "WECOM_",
    "GOOGLE_CHAT_",
    "TEAMS_",
];

/// Extract platform-prefixed env-var string literals from Rust source text.
///
/// Deliberately conservative heuristic (not a Rust parser): any quoted
/// literal that starts with a platform prefix and is entirely
/// `[A-Z0-9_]` counts — including ones appearing in comments or test code.
/// Failure direction is safe: prose in strings/comments fails the charset
/// filter and is ignored, while a var name mentioned in a comment flags
/// conservatively (fail-closed). Escaped quotes can mis-pair the scanner,
/// but real env reads are always plain literals, and garbage extractions
/// fail the charset filter.
fn platform_env_literals(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while let Some(start) = source[i..].find('"').map(|p| p + i) {
        let rest = &source[start + 1..];
        let Some(end_rel) = rest.find('"') else { break };
        let lit = &rest[..end_rel];
        if PLATFORM_PREFIXES.iter().any(|p| lit.starts_with(p))
            && lit.len() > 4
            && lit
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            found.insert(lit.to_string());
        }
        i = start + 1 + end_rel + 1;
        if i >= bytes.len() {
            break;
        }
    }
    found
}

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Scan roots: this crate, openab-core, and the binary crate. Workspace-
/// relative paths from CARGO_MANIFEST_DIR (crates/openab-gateway) — these
/// crates are not published, so the layout is stable in CI and dev trees.
fn workspace_scan_roots() -> Vec<std::path::PathBuf> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![
        here.join("src"),
        here.join("../openab-core/src"),
        here.join("../../src"),
    ]
}

fn collect_workspace_files() -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for root in workspace_scan_roots() {
        assert!(
            root.is_dir(),
            "scan root missing ({}) — workspace layout changed? update \
             workspace_scan_roots()",
            root.display()
        );
        collect_rs_files(&root, &mut files);
    }
    files
}

#[test]
fn every_platform_env_var_has_a_config_section_field() {
    let files = collect_workspace_files();
    assert!(
        !files.is_empty(),
        "no source files found — workspace layout changed?"
    );

    let covered: BTreeSet<&str> = COVERED.iter().copied().collect();
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source file");
        for var in platform_env_literals(&text) {
            if !covered.contains(var.as_str()) {
                violations.push(format!("{} (in {})", var, file.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "config-first conformance violation (#1375): the following platform \
         env vars are referenced in this crate but have NO corresponding \
         first-class [section] field in openab-core::config. Every platform \
         setting must resolve config → env → default (config always wins). \
         Fix: add the field to the platform's section struct in \
         crates/openab-core/src/config.rs, thread it through the Gateway*Config \
         bridge, document it in docs/config-reference.md, then add the var to \
         COVERED in this test.\nViolations:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn covered_set_has_no_stale_platform_entries() {
    // Symmetric hygiene check: every COVERED entry must still be referenced
    // somewhere in the workspace (at minimum, openab-core's config.rs holds
    // each section field's env fallback). Keeps the allowlist from
    // accumulating dead entries.
    let files = collect_workspace_files();
    let mut referenced = BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source file");
        referenced.extend(platform_env_literals(&text));
    }
    let stale: Vec<&str> = COVERED
        .iter()
        .copied()
        .filter(|v| {
            // Trust vars are read via dynamically constructed keys —
            // `PlatformTrustConfig::resolve_with_env` builds
            // `format!("{prefix}_ALLOW_ALL_USERS")` / `_ALLOWED_USERS` — so
            // no string literal exists for a literal scan to find. Exempt by
            // suffix; the positive-direction test still covers any literal
            // reads of them.
            !referenced.contains(*v)
                && !v.ends_with("_ALLOW_ALL_USERS")
                && !v.ends_with("_ALLOWED_USERS")
        })
        .collect();
    assert!(
        stale.is_empty(),
        "COVERED entries no longer referenced anywhere in the workspace \
         (remove them or re-wire the env fallback): {stale:?}"
    );
}

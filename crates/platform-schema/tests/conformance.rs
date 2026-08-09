//! Conformance tests for `docs/platforms/schema/*.toml`.
//!
//! Run against the real repo tree: every platform file is deserialized into the
//! authoritative structs (that IS the structural/enum check), the closed feature
//! set + schema version are enforced, and every code-ref `source` is checked to
//! still point at a file (and symbol) that exists — so the docs can't drift.

use platform_schema::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The 8 platforms that must have a schema file.
const EXPECTED_PLATFORMS: &[&str] = &[
    "line", "slack", "telegram", "discord", "feishu", "wecom", "googlechat", "teams",
];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/platform-schema  ->  up 2 = <repo>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn schema_dir() -> PathBuf {
    repo_root().join("docs/platforms/schema")
}

/// Deserialize every schema/*.toml. A parse/enum/missing-field/unknown-field
/// error panics here — that IS the structural-validity check.
fn load_all() -> Vec<(String, Platform)> {
    let dir = schema_dir();
    let mut out = Vec::new();
    let entries =
        fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path).unwrap();
        let platform: Platform = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} failed to deserialize: {e}", path.display()));
        out.push((name, platform));
    }
    out
}

#[test]
fn all_expected_platform_files_present() {
    let present: BTreeSet<String> = load_all().into_iter().map(|(n, _)| n).collect();
    let missing: Vec<_> = EXPECTED_PLATFORMS
        .iter()
        .filter(|p| !present.contains(**p))
        .collect();
    assert!(missing.is_empty(), "missing schema files for: {missing:?}");
}

#[test]
fn schema_version_is_current() {
    for (name, p) in load_all() {
        assert_eq!(
            p.schema_version, SCHEMA_VERSION,
            "{name}.toml: schema_version {} != current {SCHEMA_VERSION} (stale page)",
            p.schema_version
        );
    }
}

#[test]
fn platform_name_matches_filename() {
    for (name, p) in load_all() {
        assert_eq!(
            p.platform.name, name,
            "{name}.toml: [platform].name is {:?}, must match the filename",
            p.platform.name
        );
    }
}

#[test]
fn feature_set_is_exactly_the_closed_set() {
    let want: BTreeSet<&str> = EXPECTED_FEATURES.iter().copied().collect();
    for (name, p) in load_all() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for f in &p.openab_features {
            assert!(
                want.contains(f.feature.as_str()),
                "{name}.toml: unknown feature key {:?}",
                f.feature
            );
            assert!(
                seen.insert(f.feature.clone()),
                "{name}.toml: duplicate feature {:?}",
                f.feature
            );
        }
        let got: BTreeSet<&str> = seen.iter().map(String::as_str).collect();
        assert_eq!(
            got, want,
            "{name}.toml: feature set mismatch; missing {:?}",
            want.difference(&got).collect::<Vec<_>>()
        );
    }
}

#[test]
fn present_features_cite_a_source() {
    for (name, p) in load_all() {
        for f in &p.openab_features {
            if f.status.requires_source() {
                assert!(
                    !f.source.is_empty(),
                    "{name}.toml: feature {:?} claims presence but cites no source",
                    f.feature
                );
            }
        }
    }
}

/// The core anti-drift check: every feature code-ref points at a real file, and
/// every `#symbol` actually appears in it.
#[test]
fn feature_sources_exist_in_tree() {
    let root = repo_root();
    let mut errs = Vec::new();
    for (name, p) in load_all() {
        for f in &p.openab_features {
            for src in &f.source {
                if !is_code_ref(src) {
                    errs.push(format!(
                        "{name}.toml feature {:?}: source {src:?} is a URL, expected a file ref",
                        f.feature
                    ));
                    continue;
                }
                if let Err(msg) = check_code_ref(&root, src) {
                    errs.push(format!("{name}.toml feature {:?}: {msg}", f.feature));
                }
            }
        }
    }
    assert!(errs.is_empty(), "dead feature sources:\n  {}", errs.join("\n  "));
}

#[test]
fn quirk_code_sources_exist_in_tree() {
    let root = repo_root();
    let mut errs = Vec::new();
    for (name, p) in load_all() {
        for q in &p.quirks {
            if let Some(src) = &q.source {
                if is_code_ref(src) {
                    if let Err(msg) = check_code_ref(&root, src) {
                        errs.push(format!("{name}.toml quirk {:?}: {msg}", q.title));
                    }
                }
            }
        }
    }
    assert!(errs.is_empty(), "dead quirk sources:\n  {}", errs.join("\n  "));
}

fn check_code_ref(root: &Path, src: &str) -> Result<(), String> {
    let r = parse_code_ref(src);
    // Containment first, and lexically: a code-ref must be a plain relative path
    // inside the repo. Rejecting `..`/root/prefix components before touching the
    // filesystem keeps the guard reachable when the escaping target does not
    // exist — a canonicalize-only check can't run on a missing path, so it would
    // silently degrade to the "does not exist" error below and hide the escape.
    if Path::new(r.file)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("source path {:?} escapes repo root", r.file));
    }
    let path = root.join(r.file);
    if !path.is_file() {
        return Err(format!("source file {:?} does not exist", r.file));
    }
    // Defence in depth: a lexically-clean ref can still traverse a symlink out
    // of the tree, which only resolving both real paths catches.
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let canonical_path = path.canonicalize().map_err(|e| e.to_string())?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(format!("source path {:?} escapes repo root", r.file));
    }
    if let Some(sym) = r.symbol {
        // `trim` matters as much as the emptiness check itself: `contains("")` is
        // unconditionally true, and a whitespace-only symbol matches virtually
        // any source file. Both are vacuous passes, not anti-drift checks.
        if sym.trim().is_empty() {
            return Err(format!("empty symbol after '#' in {:?}", r.file));
        }
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if !text.contains(sym) {
            return Err(format!("symbol {sym:?} not found in {:?} (renamed/deleted?)", r.file));
        }
    }
    Ok(())
}

#[test]
fn check_code_ref_rejects_empty_symbol() {
    let root = repo_root();
    let err = check_code_ref(&root, "docs/platforms/README.md#")
        .expect_err("empty symbol must be rejected");
    assert!(err.contains("empty symbol"), "unexpected error: {err}");
}

#[test]
fn check_code_ref_rejects_whitespace_only_symbol() {
    // Same vacuous-pass class as the empty symbol: `contains(" ")` matches
    // virtually any file, so a whitespace-only `#symbol` asserts nothing.
    let root = repo_root();
    let err = check_code_ref(&root, "docs/platforms/README.md#   ")
        .expect_err("whitespace-only symbol must be rejected");
    assert!(err.contains("empty symbol"), "unexpected error: {err}");
}

#[test]
fn check_code_ref_rejects_path_traversal() {
    let root = repo_root();
    let err = check_code_ref(&root, "../../etc/passwd")
        .expect_err("path escaping repo root must be rejected");
    assert!(err.contains("escapes repo root"), "unexpected error: {err}");
}

#[test]
fn check_code_ref_rejects_absolute_path() {
    // Path::join replaces the base entirely when the joined path is absolute,
    // so this exercises a different component than the "../" traversal above.
    let root = repo_root();
    let err = check_code_ref(&root, "/etc/passwd").expect_err("absolute path must be rejected");
    assert!(err.contains("escapes repo root"), "unexpected error: {err}");
}

#[test]
fn check_code_ref_reports_escape_even_when_target_is_missing() {
    // The containment check runs before the existence check, so an escaping ref
    // is reported as an escape regardless of what happens to exist on the host.
    // That is what lets the two assertions above be exact instead of accepting
    // "does not exist" as an alternative pass.
    let root = repo_root();
    let err = check_code_ref(&root, "../no-such-file-4f21c9.rs")
        .expect_err("escaping path must be rejected");
    assert!(err.contains("escapes repo root"), "unexpected error: {err}");
}

/// The template must keep enumerating every capability section + feature key, so
/// a struct change can't silently leave the human-facing template behind.
#[test]
fn template_enumerates_every_section_and_feature() {
    let text = fs::read_to_string(repo_root().join("docs/platforms/_template.toml"))
        .expect("read _template.toml");
    for section in CAPABILITY_SECTIONS {
        let header = format!("[capability.{section}]");
        assert!(text.contains(&header), "_template.toml missing {header}");
    }
    for feature in EXPECTED_FEATURES {
        let key = format!("feature = \"{feature}\"");
        assert!(text.contains(&key), "_template.toml missing feature block for {feature}");
    }
}

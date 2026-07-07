//! Conformance tests for `docs/platforms/schema/*.toml`.
//!
//! Run against the real repo tree: every platform file is parsed + validated
//! against the schema, and every code-ref `source` is checked to still point at
//! a file (and symbol) that exists — so the docs can't silently drift.

use platform_schema::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

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

/// Parse every schema/*.toml. A syntax error panics here (that's the parse check).
fn load_all() -> Vec<(String, DocumentMut)> {
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
        let doc: DocumentMut = text
            .parse()
            .unwrap_or_else(|e| panic!("{} is not valid TOML: {e}", path.display()));
        out.push((name, doc));
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
fn every_file_conforms_to_schema() {
    let mut all_errors = Vec::new();
    for (name, doc) in load_all() {
        for err in validate(&doc, &name) {
            all_errors.push(format!("{name}.toml: {err}"));
        }
    }
    assert!(
        all_errors.is_empty(),
        "schema violations:\n  {}",
        all_errors.join("\n  ")
    );
}

/// The core anti-drift check: every feature code-ref points at a real file, and
/// every `#symbol` actually appears in it.
#[test]
fn feature_sources_exist_in_tree() {
    let root = repo_root();
    let mut errs = Vec::new();
    for (name, doc) in load_all() {
        for (ctx, src) in feature_code_refs(&doc) {
            if !is_code_ref(&src) {
                errs.push(format!("{name}.toml {ctx}: source {src:?} is a URL, expected a file ref"));
                continue;
            }
            if let Err(msg) = check_code_ref(&root, &src) {
                errs.push(format!("{name}.toml {ctx}: {msg}"));
            }
        }
    }
    assert!(errs.is_empty(), "dead feature sources:\n  {}", errs.join("\n  "));
}

#[test]
fn quirk_code_sources_exist_in_tree() {
    let root = repo_root();
    let mut errs = Vec::new();
    for (name, doc) in load_all() {
        for (ctx, src) in quirk_code_refs(&doc) {
            if let Err(msg) = check_code_ref(&root, &src) {
                errs.push(format!("{name}.toml {ctx}: {msg}"));
            }
        }
    }
    assert!(errs.is_empty(), "dead quirk sources:\n  {}", errs.join("\n  "));
}

fn check_code_ref(root: &Path, src: &str) -> Result<(), String> {
    let r = parse_code_ref(src);
    let path = root.join(r.file);
    if !path.is_file() {
        return Err(format!("source file {:?} does not exist", r.file));
    }
    if let Some(sym) = r.symbol {
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if !text.contains(sym) {
            return Err(format!("symbol {sym:?} not found in {:?} (renamed/deleted?)", r.file));
        }
    }
    Ok(())
}

/// The template must keep enumerating every capability section + feature key, so
/// a schema change can't silently leave the human-facing template behind.
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

//! Offline validation for the conservative Microsoft Teams v1.25 command menu.
//! The official schema fixture is pinned under `testdata/` so CI does not depend
//! on network availability.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const PERSONAL_COMMANDS: &[&str] = &[
    "/models",
    "/agents",
    "/cancel",
    "/cancel-all",
    "/reset",
    "/usage",
];
const SHARED_COMMANDS: &[&str] = &["/models", "/agents", "/cancel", "/cancel-all", "/reset"];

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn repo_root() -> PathBuf {
    crate_root()
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn load_json(path: &Path) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn manifest() -> Value {
    load_json(&repo_root().join("docs/platforms/examples/teams-manifest-v1.25.json"))
}

fn markdown_manifest(relative_path: &str) -> Value {
    let path = repo_root().join(relative_path);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    text.split("```json")
        .skip(1)
        .filter_map(|suffix| suffix.split("```").next())
        .filter_map(|block| serde_json::from_str::<Value>(block.trim()).ok())
        .find(|value| value["manifestVersion"] == "1.25")
        .unwrap_or_else(|| panic!("{} has no v1.25 manifest JSON block", path.display()))
}

#[test]
fn command_manifest_validates_against_official_v1_25_schema() {
    let schema = load_json(&crate_root().join("testdata/MicrosoftTeams.v1.25.schema.json"));
    let validator = jsonschema::validator_for(&schema).expect("official schema compiles");
    let manifest = manifest();
    let errors: Vec<String> = validator
        .iter_errors(&manifest)
        .map(|error| {
            let path = error.instance_path().to_string();
            if path.is_empty() {
                error.to_string()
            } else {
                format!("{path}: {error}")
            }
        })
        .collect();
    assert!(
        errors.is_empty(),
        "Teams manifest failed v1.25 validation:\n{}",
        errors.join("\n")
    );
}

#[test]
fn command_manifest_keeps_usage_private_and_permissions_conservative() {
    let manifest = manifest();
    assert_eq!(manifest["manifestVersion"], "1.25");
    assert_eq!(manifest["validDomains"], serde_json::json!([]));
    assert!(manifest.get("authorization").is_none());
    assert!(manifest.get("webApplicationInfo").is_none());

    let bots = manifest["bots"].as_array().expect("bots array");
    assert_eq!(bots.len(), 1);
    let bot = &bots[0];
    assert_eq!(bot["supportsFiles"], false);
    assert!(bot.get("supportsTargetedMessages").is_none());

    let command_lists = bot["commandLists"].as_array().expect("commandLists array");
    assert_eq!(command_lists.len(), 2);
    let personal = command_lists
        .iter()
        .find(|list| list["scopes"] == serde_json::json!(["personal"]))
        .expect("Personal command list");
    let shared = command_lists
        .iter()
        .find(|list| list["scopes"] == serde_json::json!(["team", "groupChat"]))
        .expect("Team/groupChat command list");

    assert_eq!(command_titles(personal), PERSONAL_COMMANDS);
    assert_eq!(command_titles(shared), SHARED_COMMANDS);
    assert!(!command_titles(shared).contains(&"/usage"));

    for list in command_lists {
        let commands = list["commands"].as_array().expect("commands array");
        assert!(commands.len() <= 12);
        for command in commands {
            assert!(command.get("triggers").is_none());
            assert!(command["title"]
                .as_str()
                .is_some_and(|title| title.starts_with('/')));
            assert!(command["description"]
                .as_str()
                .is_some_and(|description| !description.trim().is_empty()));
        }
    }
}

#[test]
fn setup_guides_embed_the_validated_command_lists() {
    let canonical = manifest();
    let expected = &canonical["bots"][0]["commandLists"];
    for path in ["docs/msteams-selfhosted.md", "docs/msteams-enterprise.md"] {
        let documented = markdown_manifest(path);
        assert_eq!(
            &documented["bots"][0]["commandLists"], expected,
            "{path} commandLists drifted from the validated example"
        );
        assert_eq!(documented["bots"][0]["supportsFiles"], false);
        assert!(documented["bots"][0]
            .get("supportsTargetedMessages")
            .is_none());
    }
}

fn command_titles(list: &Value) -> Vec<&str> {
    list["commands"]
        .as_array()
        .expect("commands array")
        .iter()
        .map(|command| command["title"].as_str().expect("command title"))
        .collect()
}

use fs2::FileExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::types::*;

const DEFAULT_PRINT_TIMEOUT: &str = "20m";

/// Split one `agy models` output line into (slug, display name).
/// agy 1.1.27+ emits `<model-id>\t<display name>`; older versions emit a
/// single field, which doubles as both value and name (as does the static
/// fallback list).
fn parse_model_line(line: &str) -> (String, String) {
    let (slug, name) = match line.split_once('\t') {
        Some((slug, name)) => (slug.trim(), name.trim()),
        None => (line.trim(), line.trim()),
    };
    (slug.to_string(), if name.is_empty() { slug.to_string() } else { name.to_string() })
}

/// Normalize a stored or client-sent model value down to the slug passed to
/// `agy --model`. Values recorded before tab-separated `agy models` output was
/// parsed may still hold the whole `<slug>\t<name>` line.
fn model_slug(value: &str) -> String {
    parse_model_line(value).0
}

fn prompt_extra_args(extra: &str) -> Vec<String> {
    let mut args = shell_words::split(extra).unwrap_or_else(|_| {
        eprintln!("[agy-acp] WARN: failed to parse AGY_EXTRA_ARGS, ignoring");
        Vec::new()
    });
    if !args
        .iter()
        .any(|arg| arg == "--print-timeout" || arg.starts_with("--print-timeout="))
    {
        args.push("--print-timeout".to_string());
        args.push(DEFAULT_PRINT_TIMEOUT.to_string());
    }
    args
}

pub struct Adapter {
    pub sessions: HashMap<String, Session>,
    pub working_dir: String,
    pub conversations_dir: PathBuf,
    pub state_file: PathBuf,
    pub available_models: Option<Vec<String>>,
}

impl Adapter {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let state_dir = PathBuf::from(&home).join(".openab/agy-acp");
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
            conversations_dir: PathBuf::from(&home).join(".gemini/antigravity-cli/conversations"),
            state_file: state_dir.join("sessions.json"),
            available_models: None,
        }
    }

    // --- Model cache ---

    pub fn models_cache_path(&self) -> PathBuf {
        self.state_file.with_file_name("models_cache.json")
    }

    pub fn load_cached_models(&self) -> Option<Vec<String>> {
        let path = self.models_cache_path();
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str::<Vec<String>>(&content).ok().filter(|v| !v.is_empty())
    }

    pub fn save_models_cache(&self, models: &[String]) {
        if let Some(parent) = self.models_cache_path().parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(models) {
            let tmp = self.models_cache_path().with_extension("tmp");
            if fs::write(&tmp, &json).is_ok() {
                let _ = fs::rename(&tmp, self.models_cache_path());
            }
        }
    }

    pub fn static_fallback_models() -> Vec<String> {
        vec![
            "Gemini 3.5 Flash (Medium)".to_string(),
            "Gemini 3.5 Flash (High)".to_string(),
            "Gemini 3.5 Flash (Low)".to_string(),
            "Gemini 3.1 Pro (Low)".to_string(),
            "Gemini 3.1 Pro (High)".to_string(),
        ]
    }

    /// Resolve the `agy` binary path.
    pub fn agy_bin() -> &'static str {
        "/usr/local/bin/agy"
    }

    /// Build PATH with common agent binary locations prepended.
    pub fn augmented_path() -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".to_string());
        let base = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
        format!("{home}/bin:{home}/.local/bin:{home}/.local/share/fnm/aliases/default/bin:{base}")
    }

    pub fn fetch_available_models() -> Vec<String> {
        std::process::Command::new(Self::agy_bin())
            .arg("models")
            .env("PATH", Self::augmented_path())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_available_models(&mut self) -> &[String] {
        if self.available_models.is_none() {
            let models = Self::fetch_available_models();
            if !models.is_empty() {
                eprintln!("[agy-acp] fetched {} models from `agy models`, updating cache", models.len());
                self.save_models_cache(&models);
                self.available_models = Some(models);
            } else if let Some(cached) = self.load_cached_models() {
                eprintln!("[agy-acp] `agy models` failed, using cached model list ({} models)", cached.len());
                self.available_models = Some(cached);
            } else {
                eprintln!("[agy-acp] `agy models` failed and no cache found, using hardcoded fallback");
                self.available_models = Some(Self::static_fallback_models());
            }
        }
        self.available_models.as_ref().unwrap()
    }

    pub fn config_options_json(&mut self, model_id: Option<&str>) -> Value {
        let parsed: Vec<(String, String)> = self.get_available_models().iter()
            .map(|line| parse_model_line(line))
            .filter(|(slug, _)| !slug.is_empty())
            .collect();
        if parsed.is_empty() {
            return json!([]);
        }
        let current = model_id
            .map(model_slug)
            .filter(|slug| !slug.is_empty())
            .unwrap_or_else(|| parsed[0].0.clone());
        let options: Vec<Value> = parsed
            .iter()
            .map(|(slug, name)| json!({ "value": slug, "name": name }))
            .collect();
        json!([{
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current,
            "options": options,
        }])
    }

    // --- State persistence ---

    fn lock_state_file(&self) -> Option<fs::File> {
        if let Some(parent) = self.state_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let lock_path = self.state_file.with_extension("lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .ok()?;
        lock_file.lock_exclusive().ok()?;
        Some(lock_file)
    }

    fn load_store_inner(&self) -> SessionStore {
        let Some(file) = fs::File::open(&self.state_file).ok() else {
            return SessionStore::default();
        };
        serde_json::from_reader(&file).unwrap_or_default()
    }

    pub fn load_store(&self) -> SessionStore {
        let _lock = self.lock_state_file();
        self.load_store_inner()
    }

    pub fn restore_session(&self, session_id: &str) -> Option<(String, i64, Option<String>)> {
        let store = self.load_store();
        store.sessions.get(session_id).and_then(|s| {
            // Sessions persisted before tab-separated `agy models` output was
            // parsed may hold the whole `<slug>\t<name>` line; normalize so
            // in-memory state (and the next persist) carries only the slug.
            let model_id = s.model_id.clone().map(|m| model_slug(&m)).filter(|m| !m.is_empty());
            s.conversation_id.clone().map(|cid| (cid, s.last_step_idx, model_id))
        })
    }

    pub fn persist_session(&self, session_id: &str, conversation_id: Option<&str>, last_step_idx: i64, model_id: Option<&str>) {
        let Some(_lock) = self.lock_state_file() else { return; };
        let mut store = self.load_store_inner();
        store.sessions.insert(
            session_id.to_string(),
            StoredSession {
                conversation_id: conversation_id.map(String::from),
                last_step_idx,
                model_id: model_id.map(String::from),
            },
        );
        let tmp = self.state_file.with_extension("tmp");
        if let Ok(file) = fs::File::create(&tmp) {
            if serde_json::to_writer_pretty(&file, &store).is_ok() {
                let _ = fs::rename(&tmp, &self.state_file);
            }
        }
    }

    // --- Conversation snapshot ---

    pub fn conversation_snapshot(&self) -> HashSet<String> {
        let Ok(entries) = fs::read_dir(&self.conversations_dir) else {
            return HashSet::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().map(|x| x == "db").unwrap_or(false) {
                    path.file_stem().map(|s| s.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn new_conversation_id(&self, before: &HashSet<String>) -> Option<String> {
        let after = self.conversation_snapshot();
        let mut created: Vec<_> = after.difference(before).collect();
        if created.is_empty() { return None; }
        if created.len() > 1 {
            eprintln!("[agy-acp] WARN: multiple new agy conversation files appeared; refusing to bind");
            return None;
        }
        Some(created.remove(0).clone())
    }

    // --- Session management ---

    pub fn evict_if_needed(&mut self) {
        const MAX_SESSIONS: usize = 64;
        while self.sessions.len() >= MAX_SESSIONS {
            if let Some(key) = self.sessions.keys().next().cloned() {
                self.sessions.remove(&key);
            }
        }
    }

    pub fn restore_session_state(&mut self, session_id: &str) -> bool {
        let Some((conversation_id, last_step_idx, model_id)) = self.restore_session(session_id) else {
            return false;
        };
        if !self.sessions.contains_key(session_id) {
            self.evict_if_needed();
        }
        self.sessions.insert(
            session_id.to_string(),
            Session { conversation_id: Some(conversation_id), last_step_idx, model_id },
        );
        true
    }

    // --- JSON-RPC handlers ---

    pub fn handle_initialize(&self, id: Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "agy", "version": env!("CARGO_PKG_VERSION") },
                "agentCapabilities": { "streaming": true, "loadSession": true },
            })),
            error: None,
        }
    }

    pub fn handle_session_new(&mut self, id: Value) -> JsonRpcResponse {
        let session_id = Uuid::new_v4().to_string();
        self.evict_if_needed();
        self.sessions.insert(session_id.clone(), Session {
            conversation_id: None, last_step_idx: -1, model_id: None,
        });
        let config_options = self.config_options_json(None);
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "sessionId": session_id, "configOptions": config_options })),
            error: None,
        }
    }

    pub fn handle_session_load(&mut self, id: Value, params: &Value) -> JsonRpcResponse {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if session_id.is_empty() {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32602,"message":"missing sessionId"})) };
        }
        if self.restore_session_state(session_id) {
            let model_id = self.sessions.get(session_id).and_then(|s| s.model_id.clone());
            let config_options = self.config_options_json(model_id.as_deref());
            return JsonRpcResponse { jsonrpc: "2.0", id,
                result: Some(json!({ "sessionId": session_id, "configOptions": config_options })), error: None };
        }
        JsonRpcResponse { jsonrpc: "2.0", id, result: None,
            error: Some(json!({"code":-32000,"message":format!("unknown sessionId: {session_id}")})) }
    }

    pub fn handle_session_set_config_option(&mut self, id: Value, params: &Value) -> JsonRpcResponse {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        let config_id = params.get("configId").and_then(|v| v.as_str()).unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
        let slug = model_slug(value);

        if session_id.is_empty() || config_id != "model" || slug.is_empty() {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32602,"message":"missing sessionId, configId, or value"})) };
        }
        if !self.sessions.contains_key(session_id) {
            let _ = self.restore_session_state(session_id);
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32000,"message":format!("unknown sessionId: {session_id}")})) };
        };
        session.model_id = Some(slug.clone());
        let conv_id = session.conversation_id.clone();
        let last_step_idx = session.last_step_idx;
        self.persist_session(session_id, conv_id.as_deref(), last_step_idx, Some(&slug));
        let config_options = self.config_options_json(Some(&slug));
        JsonRpcResponse { jsonrpc: "2.0", id, result: Some(json!({ "configOptions": config_options })), error: None }
    }

    /// Gather session state needed for prompt execution (under lock).
    pub fn prepare_prompt_state(
        &mut self,
        params: &Value,
    ) -> (String, String, Vec<String>, Option<HashSet<String>>, Option<String>, i64) {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();

        if !session_id.is_empty() && !self.sessions.contains_key(&session_id) {
            let _ = self.restore_session_state(&session_id);
        }

        let prompt_text = params
            .get("prompt")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let clean_prompt = prompt_text.trim().to_string();

        let snapshot = if self.sessions.get(&session_id).map(|s| s.conversation_id.is_none()).unwrap_or(false) {
            Some(self.conversation_snapshot())
        } else {
            None
        };

        let mut args: Vec<String> = Vec::new();
        args.push("--add-dir".to_string());
        args.push(self.working_dir.clone());
        args.extend(prompt_extra_args(&std::env::var("AGY_EXTRA_ARGS").unwrap_or_default()));
        if let Some(session) = self.sessions.get(&session_id) {
            if let Some(conv_id) = &session.conversation_id {
                args.push("--conversation".to_string());
                args.push(conv_id.clone());
            }
            if let Some(model_id) = &session.model_id {
                let slug = model_slug(model_id);
                if !slug.is_empty() {
                    args.push("--model".to_string());
                    args.push(slug);
                }
            }
        }
        args.push("-p".to_string());
        args.push(clean_prompt.clone());

        let initial_conv_id = self.sessions.get(&session_id).and_then(|s| s.conversation_id.clone());
        let initial_step_idx = self.sessions.get(&session_id).map(|s| s.last_step_idx).unwrap_or(-1);

        (session_id, clean_prompt, args, snapshot, initial_conv_id, initial_step_idx)
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_model_line, prompt_extra_args, Adapter};
    use crate::types::Session;
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use uuid::Uuid;

    /// RAII guard for a temp-dir test fixture: removes the directory on drop,
    /// so cleanup still runs if an assertion panics mid-test.
    struct TempDirGuard(std::path::PathBuf);
    impl std::ops::Deref for TempDirGuard {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path { &self.0 }
    }
    impl AsRef<std::path::Path> for TempDirGuard {
        fn as_ref(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDirGuard {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    fn adapter_with_models(root: &std::path::Path, models: Vec<String>) -> Adapter {
        Adapter {
            sessions: HashMap::new(),
            working_dir: root.to_string_lossy().to_string(),
            conversations_dir: root.join("conversations"),
            state_file: root.join("sessions.json"),
            available_models: Some(models),
        }
    }

    #[test]
    fn parse_model_line_splits_tab_separated_slug_and_name() {
        assert_eq!(
            parse_model_line("gemini-3.8-flash-high\tGemini 3.8 Flash (High)"),
            ("gemini-3.8-flash-high".to_string(), "Gemini 3.8 Flash (High)".to_string())
        );
    }

    #[test]
    fn parse_model_line_legacy_single_field_doubles_as_name() {
        assert_eq!(
            parse_model_line("Gemini 3.5 Flash (Medium)"),
            ("Gemini 3.5 Flash (Medium)".to_string(), "Gemini 3.5 Flash (Medium)".to_string())
        );
    }

    #[test]
    fn parse_model_line_empty_display_name_falls_back_to_slug() {
        assert_eq!(
            parse_model_line("gemini-3.8-flash-high\t"),
            ("gemini-3.8-flash-high".to_string(), "gemini-3.8-flash-high".to_string())
        );
        assert_eq!(
            parse_model_line("gemini-3.8-flash-high\t   "),
            ("gemini-3.8-flash-high".to_string(), "gemini-3.8-flash-high".to_string())
        );
    }

    #[test]
    fn parse_model_line_trims_fields() {
        assert_eq!(
            parse_model_line("  gemini-3.8-flash-high \t Gemini 3.8 Flash (High) "),
            ("gemini-3.8-flash-high".to_string(), "Gemini 3.8 Flash (High)".to_string())
        );
    }

    #[test]
    fn config_options_split_tab_separated_lines_into_value_and_name() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-cfgopt-tab-{}", Uuid::new_v4())));
        let mut adapter = adapter_with_models(&root, vec![
            "gemini-3.8-flash-high\tGemini 3.8 Flash (High)".to_string(),
            "gemini-3.1-pro-low\tGemini 3.1 Pro (Low)".to_string(),
        ]);
        let options = adapter.config_options_json(None);
        let model = &options[0];
        assert_eq!(model["currentValue"], json!("gemini-3.8-flash-high"));
        let opts = model["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0], json!({"value": "gemini-3.8-flash-high", "name": "Gemini 3.8 Flash (High)"}));
        assert_eq!(opts[1], json!({"value": "gemini-3.1-pro-low", "name": "Gemini 3.1 Pro (Low)"}));
        for o in opts {
            assert!(!o["value"].as_str().unwrap().contains('\t'), "option value must be a bare slug: {o}");
        }
    }

    #[test]
    fn config_options_legacy_single_field_lines_keep_value_and_name_identical() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-cfgopt-legacy-{}", Uuid::new_v4())));
        let mut adapter = adapter_with_models(&root, Adapter::static_fallback_models());
        let options = adapter.config_options_json(None);
        let opts = options[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 5);
        assert_eq!(opts[0], json!({"value": "Gemini 3.5 Flash (Medium)", "name": "Gemini 3.5 Flash (Medium)"}));
        assert_eq!(options[0]["currentValue"], json!("Gemini 3.5 Flash (Medium)"));
    }

    #[test]
    fn config_options_normalizes_legacy_persisted_current_value() {
        // Sessions persisted before tab-separated output was parsed may hold the
        // whole `<slug>\t<name>` line as model_id; currentValue must still be a
        // bare slug.
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-cfgopt-cur-{}", Uuid::new_v4())));
        let mut adapter = adapter_with_models(&root, vec![
            "gemini-3.8-flash-high\tGemini 3.8 Flash (High)".to_string(),
        ]);
        let options = adapter.config_options_json(Some("gemini-3.8-flash-high\tGemini 3.8 Flash (High)"));
        assert_eq!(options[0]["currentValue"], json!("gemini-3.8-flash-high"));
    }

    #[test]
    fn config_options_drops_lines_without_a_slug() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-cfgopt-noslug-{}", Uuid::new_v4())));
        let mut adapter = adapter_with_models(&root, vec!["\tNo slug here".to_string()]);
        assert_eq!(adapter.config_options_json(None), json!([]));
    }

    #[test]
    #[ignore]
    fn set_config_option_persists_only_the_slug() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-setcfg-{}", Uuid::new_v4())));
        let _ = fs::create_dir_all(&root);
        let mut adapter = adapter_with_models(&root, vec![
            "gemini-3.8-flash-high\tGemini 3.8 Flash (High)".to_string(),
        ]);
        adapter.sessions.insert("sess-1".to_string(), Session {
            conversation_id: Some("conv-1".to_string()), last_step_idx: -1, model_id: None,
        });
        // A client may still send a whole tab-separated record (cached picker
        // value or operator-provided default_config_options); only the slug is
        // kept.
        let response = adapter.handle_session_set_config_option(json!(9), &json!({
            "sessionId": "sess-1", "configId": "model",
            "value": "gemini-3.8-flash-high\tGemini 3.8 Flash (High)"
        }));
        assert!(response.error.is_none());
        assert_eq!(adapter.sessions["sess-1"].model_id.as_deref(), Some("gemini-3.8-flash-high"));
        assert_eq!(
            adapter.restore_session("sess-1"),
            Some(("conv-1".to_string(), -1, Some("gemini-3.8-flash-high".to_string())))
        );
        let result = response.result.unwrap();
        assert_eq!(result["configOptions"][0]["currentValue"], json!("gemini-3.8-flash-high"));
    }

    #[test]
    #[ignore]
    fn set_config_option_rejects_value_without_a_slug() {
        // `"\tname"` normalizes to an empty slug — reject like an empty value.
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-setcfg-empty-{}", Uuid::new_v4())));
        let _ = fs::create_dir_all(&root);
        let mut adapter = adapter_with_models(&root, vec![]);
        adapter.sessions.insert("sess-1".to_string(), Session {
            conversation_id: Some("conv-1".to_string()), last_step_idx: -1, model_id: None,
        });
        let response = adapter.handle_session_set_config_option(json!(9), &json!({
            "sessionId": "sess-1", "configId": "model", "value": "\tOnly Name"
        }));
        assert!(response.error.is_some());
        assert!(adapter.sessions["sess-1"].model_id.is_none());
    }

    #[test]
    #[ignore]
    fn restore_session_normalizes_legacy_persisted_model_id() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-restore-{}", Uuid::new_v4())));
        let _ = fs::create_dir_all(&root);
        let adapter = adapter_with_models(&root, vec![]);
        adapter.persist_session(
            "sess-1", Some("conv-1"), 7,
            Some("gemini-3.8-flash-high\tGemini 3.8 Flash (High)"),
        );
        assert_eq!(
            adapter.restore_session("sess-1"),
            Some(("conv-1".to_string(), 7, Some("gemini-3.8-flash-high".to_string())))
        );
    }

    #[test]
    fn prompt_args_pass_a_single_slug_to_the_model_flag() {
        let root = TempDirGuard(std::env::temp_dir().join(format!("agy-acp-prompt-model-{}", Uuid::new_v4())));
        let mut adapter = adapter_with_models(&root, vec![]);
        adapter.sessions.insert("sess-1".to_string(), Session {
            conversation_id: Some("conv-1".to_string()), last_step_idx: 0,
            // Legacy-persisted value: whole tab-separated record.
            model_id: Some("gemini-3.8-flash-high\tGemini 3.8 Flash (High)".to_string()),
        });
        let (_sid, _prompt, args, _snap, _conv, _idx) = adapter.prepare_prompt_state(&json!({
            "sessionId": "sess-1", "prompt": [{"type": "text", "text": "hi"}]
        }));
        let positions: Vec<usize> = args.iter().enumerate()
            .filter(|(_, a)| *a == "--model").map(|(i, _)| i).collect();
        assert_eq!(positions.len(), 1, "expected exactly one --model flag in {args:?}");
        assert_eq!(args[positions[0] + 1], "gemini-3.8-flash-high");
        assert!(args.iter().all(|a| !a.contains('\t')), "no arg may contain a tab: {args:?}");
    }

    #[test]
    fn default_timeout_is_added_without_discarding_extra_args() {
        assert_eq!(prompt_extra_args(""), ["--print-timeout", "20m"]);
        assert_eq!(
            prompt_extra_args("--model 'model with spaces'"),
            ["--model", "model with spaces", "--print-timeout", "20m"]
        );
    }

    #[test]
    fn explicit_timeout_is_preserved_in_both_forms() {
        assert_eq!(
            prompt_extra_args("--print-timeout 5m"),
            ["--print-timeout", "5m"]
        );
        assert_eq!(
            prompt_extra_args("--print-timeout=30m"),
            ["--print-timeout=30m"]
        );
    }

    #[test]
    fn invalid_explicit_timeouts_are_left_for_cli_validation() {
        for extra in [
            "--print-timeout",
            "--print-timeout=",
            "--print-timeout ''",
            "--print-timeout --model example",
            "--print-timeout=invalid",
        ] {
            assert_eq!(prompt_extra_args(extra), shell_words::split(extra).unwrap());
        }
    }

    #[test]
    fn similar_flag_does_not_suppress_default() {
        assert_eq!(
            prompt_extra_args("--print-timeout-other 1s"),
            ["--print-timeout-other", "1s", "--print-timeout", "20m"]
        );
    }

    #[test]
    fn malformed_extra_args_still_get_default_timeout() {
        assert_eq!(
            prompt_extra_args("--print-timeout '5m"),
            ["--print-timeout", "20m"]
        );
    }
}

use fs2::FileExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::types::*;

// Default print timeout raised from the stock 20m to 60m so slow model
// turns aren't killed before the harness's own (60m) idle window fires.
// Still overridable per-invocation via AGY_EXTRA_ARGS / --print-timeout.
const DEFAULT_PRINT_TIMEOUT: &str = "60m";

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
        // State dir is configurable via AGY_ACP_STATE_DIR (so a harness can own
        // it, e.g. ~/.vibe-station/agy-acp/); defaults to ~/.openab/agy-acp.
        let state_dir = std::env::var("AGY_ACP_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".openab/agy-acp"));
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

    /// Resolve the `agy` binary path. Honors the `AGY_BIN` env override so a
    /// harness can point the adapter at the user's own `agy` install; falls
    /// back to the conventional location.
    pub fn agy_bin() -> String {
        std::env::var("AGY_BIN").unwrap_or_else(|_| "/usr/local/bin/agy".to_string())
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
        let models = self.get_available_models();
        if models.is_empty() {
            return json!([]);
        }
        let current = model_id
            .or_else(|| models.first().map(|s| s.as_str()))
            .unwrap_or("");
        let options: Vec<Value> = models
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
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
            s.conversation_id.clone().map(|cid| (cid, s.last_step_idx, s.model_id.clone()))
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

        if session_id.is_empty() || config_id != "model" || value.is_empty() {
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
        session.model_id = Some(value.to_string());
        let conv_id = session.conversation_id.clone();
        let last_step_idx = session.last_step_idx;
        self.persist_session(session_id, conv_id.as_deref(), last_step_idx, Some(value));
        let config_options = self.config_options_json(Some(value));
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
                args.push("--model".to_string());
                args.push(model_id.clone());
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
    use super::prompt_extra_args;

    #[test]
    fn default_timeout_is_added_without_discarding_extra_args() {
        assert_eq!(prompt_extra_args(""), ["--print-timeout", "60m"]);
        assert_eq!(
            prompt_extra_args("--model 'model with spaces'"),
            ["--model", "model with spaces", "--print-timeout", "60m"]
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
            ["--print-timeout-other", "1s", "--print-timeout", "60m"]
        );
    }

    #[test]
    fn malformed_extra_args_still_get_default_timeout() {
        assert_eq!(
            prompt_extra_args("--print-timeout '5m"),
            ["--print-timeout", "60m"]
        );
    }
}

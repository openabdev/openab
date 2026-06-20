use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use base64::{Engine as _, engine::general_purpose};

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: Option<String>,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

/// Persisted session→conversation mapping stored in ~/.openab/agy-acp/sessions.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionStore {
    pub sessions: HashMap<String, StoredSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub last_step_idx: i64,
    #[serde(default)]
    pub model_id: Option<String>,
}

pub struct Session {
    pub conversation_id: Option<String>,
    pub last_step_idx: i64,
    pub model_id: Option<String>,
}

/// Tracks streaming poll state shared between the polling thread and main task.
pub struct StreamingState {
    pub conversation_id: Option<String>,
    pub base_step_idx: i64,
    pub last_step_idx: i64,
    pub emitted_len: HashMap<i64, usize>,
    pub emitted_tool_steps: HashSet<i64>,
    pub had_updates: bool,
}

/// Output from prompt execution (used to separate lock-free execution from state update).
pub struct PromptOutput {
    pub response_lines: Vec<String>,
    pub session_update: Option<(Option<String>, i64)>,
}

/// Drop guard that sets stop_polling flag when the future is dropped (task abort safety).
pub struct StopGuard(pub Arc<AtomicBool>);
impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn parse_prompt(prompt_val: Option<&Value>) -> String {
    let Some(val) = prompt_val else {
        return String::new();
    };
    if let Some(s) = val.as_str() {
        return s.to_string();
    }
    if let Some(arr) = val.as_array() {
        return parse_prompt_blocks(arr);
    }
    String::new()
}

pub fn parse_prompt_blocks(arr: &[Value]) -> String {
    let mut prompt_text = String::new();
    for block in arr {
        if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        prompt_text.push_str(text);
                    }
                }
                "file" | "resource_link" => {
                    if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                        let mut prefix = "@";
                        if prompt_text.ends_with('@') {
                            prefix = "";
                        }
                        if let Some(path) = uri.strip_prefix("file://") {
                            prompt_text.push_str(&format!("{}{}", prefix, path));
                        } else {
                            prompt_text.push_str(&format!("{}{}", prefix, uri));
                        }
                    }
                }
                "resource" => {
                    if let Some(content) = block.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()) {
                        prompt_text.push_str(content);
                    } else if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                        let mut prefix = "@";
                        if prompt_text.ends_with('@') {
                            prefix = "";
                        }
                        if let Some(path) = uri.strip_prefix("file://") {
                            prompt_text.push_str(&format!("{}{}", prefix, path));
                        } else {
                            prompt_text.push_str(&format!("{}{}", prefix, uri));
                        }
                    }
                }
                _ => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        prompt_text.push_str(text);
                    }
                }
            }
        } else {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                prompt_text.push_str(text);
            }
        }
    }
    prompt_text
}

pub fn parse_prompt_and_extract_media(prompt_val: Option<&Value>) -> (String, Vec<String>) {
    let mut temp_files = Vec::new();
    let Some(val) = prompt_val else {
        return (String::new(), temp_files);
    };
    if let Some(s) = val.as_str() {
        return (s.to_string(), temp_files);
    }
    let Some(arr) = val.as_array() else {
        return (String::new(), temp_files);
    };

    let mut prompt_text = String::new();
    let temp_dir_path = std::path::PathBuf::from("/tmp/agy-media");

    for block in arr {
        if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        prompt_text.push_str(text);
                    }
                }
                "file" | "resource_link" => {
                    if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                        let mut prefix = "@";
                        if prompt_text.ends_with('@') {
                            prefix = "";
                        }
                        if let Some(path) = uri.strip_prefix("file://") {
                            prompt_text.push_str(&format!("{}{}", prefix, path));
                        } else {
                            prompt_text.push_str(&format!("{}{}", prefix, uri));
                        }
                    }
                }
                "resource" => {
                    if let Some(content) = block.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()) {
                        prompt_text.push_str(content);
                    } else if let Some(uri) = block.get("uri").and_then(|u| u.as_str()) {
                        let mut prefix = "@";
                        if prompt_text.ends_with('@') {
                            prefix = "";
                        }
                        if let Some(path) = uri.strip_prefix("file://") {
                            prompt_text.push_str(&format!("{}{}", prefix, path));
                        } else {
                            prompt_text.push_str(&format!("{}{}", prefix, uri));
                        }
                    }
                }
                "image" => {
                    if let Some(base64_data) = block.get("data").and_then(|d| d.as_str()) {
                        let mime_type = block.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png");
                        let ext = match mime_type {
                            "image/jpeg" | "image/jpg" => "jpg",
                            "image/webp" => "webp",
                            "image/gif" => "gif",
                            _ => "png",
                        };

                        if let Some(bytes) = base64_decode(base64_data) {
                            let _ = std::fs::create_dir_all(&temp_dir_path);
                            let rand_suffix: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
                            let temp_file_name = format!("temp_media_{}.{}", rand_suffix, ext);
                            let temp_file_path = temp_dir_path.join(&temp_file_name);
                            if std::fs::write(&temp_file_path, &bytes).is_ok() {
                                let path_str = temp_file_path.to_string_lossy().to_string();
                                prompt_text.push_str(&format!(" ![{}]({}) ", temp_file_name, path_str));
                                temp_files.push(path_str);
                            }
                        }
                    }
                }
                _ => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        prompt_text.push_str(text);
                    }
                }
            }
        } else {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                prompt_text.push_str(text);
            }
        }
    }
    (prompt_text, temp_files)
}

pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // data:image/png;base64,... gibi bir ön ek varsa temizle
    let clean_input = if let Some(comma_idx) = input.find(',') {
        &input[comma_idx + 1..]
    } else {
        input
    };

    // Boşlukları, yeni satırları ve '=' karakterlerini temizle (padding'i kendimiz ekleyeceğiz)
    let mut clean_input_str: String = clean_input
        .chars()
        .filter(|c| !c.is_whitespace() && c != &'=')
        .collect();

    // 4'ün katı olana kadar '=' ekle
    while clean_input_str.len() % 4 != 0 {
        clean_input_str.push('=');
    }

    general_purpose::STANDARD.decode(clean_input_str).ok()
}


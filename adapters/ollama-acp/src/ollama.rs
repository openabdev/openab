//! Ollama HTTP client — streams chat completions via SSE.

use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl OllamaConfig {
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("OLLAMA_BASE_URL")
                .or_else(|_| std::env::var("LLM_BASE_URL"))
                .unwrap_or_else(|_| "http://localhost:11434/v1".into()),
            model: std::env::var("OLLAMA_MODEL")
                .or_else(|_| std::env::var("LLM_MODEL"))
                .unwrap_or_else(|_| "gemma4:26b".into()),
            api_key: std::env::var("OLLAMA_API_KEY")
                .or_else(|_| std::env::var("LLM_API_KEY"))
                .unwrap_or_else(|_| "ollama".into()),
        }
    }
}

#[derive(Debug)]
pub enum StreamChunk {
    Content(String),
    Error(String),
    Done,
}

/// Stream chat completion from Ollama's OpenAI-compatible endpoint.
/// Returns a channel receiver that yields chunks as they arrive.
pub async fn stream_chat(
    config: &OllamaConfig,
    messages: &[Value],
) -> Result<mpsc::Receiver<StreamChunk>, String> {
    let url = format!("{}/chat/completions", config.base_url);
    let client = Client::new();

    let body = json!({
        "model": config.model,
        "messages": messages,
        "stream": true,
    });

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Ollama HTTP {}: {}",
            response.status(),
            response
                .status()
                .canonical_reason()
                .unwrap_or("Unknown error")
        ));
    }

    let (tx, rx) = mpsc::channel(256);

    // Spawn a task to read the SSE stream using chunk()
    tokio::spawn(async move {
        let mut response = response;
        let mut buffer = String::new();

        loop {
            let chunk_result: Result<Option<bytes::Bytes>, reqwest::Error> = response.chunk().await;
            match chunk_result {
                Ok(Some(bytes)) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));

                    // Process complete lines
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim().to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        if line.is_empty() || !line.starts_with("data: ") {
                            continue;
                        }

                        let data = &line[6..];
                        if data == "[DONE]" {
                            let _ = tx.send(StreamChunk::Done).await;
                            return;
                        }

                        if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                            if let Some(text) = parsed
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(|t| t.as_str())
                            {
                                if !text.is_empty() {
                                    let _ = tx.send(StreamChunk::Content(text.to_string())).await;
                                }
                            }
                        }
                    }
                }
                Ok(None) => break,  // Stream ended
                Err(e) => {
                    let _ = tx.send(StreamChunk::Error(e.to_string())).await;
                    break;
                }
            }
        }

        let _ = tx.send(StreamChunk::Done).await;
    });

    Ok(rx)
}

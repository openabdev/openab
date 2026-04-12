use crate::config::SttConfig;
use reqwest::multipart;
use tracing::{debug, error};

/// Transcribe audio bytes via an OpenAI-compatible `/audio/transcriptions` endpoint.
pub async fn transcribe(
    client: &reqwest::Client,
    cfg: &SttConfig,
    audio_bytes: Vec<u8>,
    filename: String,
    mime_type: &str,
) -> Option<String> {
    let url = format!("{}/audio/transcriptions", cfg.base_url.trim_end_matches('/'));

    let file_part = multipart::Part::bytes(audio_bytes)
        .file_name(filename)
        .mime_str(mime_type)
        .ok()?;

    let form = multipart::Form::new()
        .part("file", file_part)
        .text("model", cfg.model.clone())
        .text("response_format", "json");

    let resp = match client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .multipart(form)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "STT request failed");
            return None;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        error!(status = %status, body = %body, "STT API error");
        return None;
    }

    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            error!(error = %e, "STT response parse failed");
            return None;
        }
    };

    let text = json.get("text")?.as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }

    debug!(chars = text.len(), "STT transcription complete");
    Some(text)
}

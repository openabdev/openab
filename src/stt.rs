use crate::adapter::{ChannelRef, ChatAdapter, MessageRef};
use crate::config::SttConfig;
use reqwest::multipart;
use std::sync::Arc;
use tracing::{debug, error, warn};

/// Outcome of attempting STT on a single audio attachment.
/// Used by adapters to feed `post_echo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EchoEntry {
    Success(String),
    Failed,
}

/// Render a list of echo entries as a single multi-line quoted block.
/// Returns `None` for empty input so callers can short-circuit.
///
/// Each entry produces one `> 🎤 …` line. Internal newlines inside a
/// transcript are flattened to spaces so each entry occupies exactly one
/// visual line — Discord and Slack both stop applying `>` at the next `\n`.
pub fn format_echo_message(entries: &[EchoEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            EchoEntry::Success(text) => {
                let flat = text.replace(['\n', '\r'], " ");
                lines.push(format!("> 🎤 {flat}"));
            }
            EchoEntry::Failed => {
                lines.push("> 🎤 (transcription failed)".to_string());
            }
        }
    }
    Some(lines.join("\n"))
}

/// Post a transcript echo to the thread and add a ⚠️ reaction for any failed
/// entries. No-op when the config disables echoing or when `entries` is empty.
///
/// Errors from the adapter (send/reaction) are logged and swallowed — the
/// echo is best-effort and must never block the agent reply.
pub async fn post_echo(
    adapter: &Arc<dyn ChatAdapter>,
    thread: &ChannelRef,
    trigger: &MessageRef,
    entries: &[EchoEntry],
    cfg: &SttConfig,
) {
    if !cfg.echo_transcript {
        return;
    }
    let Some(body) = format_echo_message(entries) else {
        return;
    };
    if let Err(e) = adapter.send_message(thread, &body).await {
        warn!(error = %e, platform = adapter.platform(), "failed to send STT echo message");
    }
    for entry in entries {
        if matches!(entry, EchoEntry::Failed) {
            if let Err(e) = adapter.add_reaction(trigger, "⚠️").await {
                warn!(error = %e, platform = adapter.platform(), "failed to add STT failure reaction");
            }
            // Add only one reaction even with multiple failures — emoji reactions
            // are unique per (user, emoji, message), so additional calls are no-ops.
            break;
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_single_success_entry() {
        let entries = vec![EchoEntry::Success("hello world".into())];
        let out = format_echo_message(&entries).expect("non-empty input → Some");
        assert_eq!(out, "> 🎤 hello world");
    }

    #[test]
    fn format_single_failure_entry() {
        let entries = vec![EchoEntry::Failed];
        let out = format_echo_message(&entries).expect("non-empty input → Some");
        assert_eq!(out, "> 🎤 (transcription failed)");
    }

    #[test]
    fn format_multiple_mixed_entries() {
        let entries = vec![
            EchoEntry::Success("first".into()),
            EchoEntry::Failed,
            EchoEntry::Success("third".into()),
        ];
        let out = format_echo_message(&entries).expect("non-empty input → Some");
        assert_eq!(out, "> 🎤 first\n> 🎤 (transcription failed)\n> 🎤 third");
    }

    #[test]
    fn format_empty_entries_returns_none() {
        let entries: Vec<EchoEntry> = vec![];
        assert!(format_echo_message(&entries).is_none());
    }

    #[test]
    fn format_strips_internal_newlines_in_transcript() {
        // Multi-line transcripts must collapse to a single quoted line so the
        // ">" prefix still applies to every visual line.
        let entries = vec![EchoEntry::Success("line one\nline two".into())];
        let out = format_echo_message(&entries).expect("non-empty input → Some");
        assert_eq!(out, "> 🎤 line one line two");
    }

    use crate::adapter::{ChannelRef, ChatAdapter, MessageRef};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockAdapter {
        sent_messages: Mutex<Vec<(ChannelRef, String)>>,
        reactions: Mutex<Vec<(MessageRef, String)>>,
    }

    #[async_trait]
    impl ChatAdapter for MockAdapter {
        fn platform(&self) -> &'static str { "mock" }
        fn message_limit(&self) -> usize { 4000 }
        async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
            self.sent_messages.lock().unwrap().push((channel.clone(), content.to_string()));
            Ok(MessageRef { channel: channel.clone(), message_id: "mock-msg".into() })
        }
        async fn create_thread(&self, channel: &ChannelRef, _trigger: &MessageRef, _title: &str) -> Result<ChannelRef> {
            Ok(channel.clone())
        }
        async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
            self.reactions.lock().unwrap().push((msg.clone(), emoji.to_string()));
            Ok(())
        }
        async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> { Ok(()) }
        fn use_streaming(&self, _other_bot_present: bool) -> bool { false }
    }

    fn test_channel() -> ChannelRef {
        ChannelRef {
            platform: "mock".into(),
            channel_id: "C1".into(),
            thread_id: Some("T1".into()),
            parent_id: None,
        }
    }

    fn test_trigger() -> MessageRef {
        MessageRef { channel: test_channel(), message_id: "M1".into() }
    }

    fn cfg(echo: bool) -> SttConfig {
        SttConfig { echo_transcript: echo, ..SttConfig::default() }
    }

    #[tokio::test]
    async fn post_echo_success_sends_one_message_no_reactions() {
        let mock = Arc::new(MockAdapter::default());
        let adapter: Arc<dyn ChatAdapter> = mock.clone();
        let entries = vec![EchoEntry::Success("hello".into())];
        post_echo(&adapter, &test_channel(), &test_trigger(), &entries, &cfg(true)).await;

        assert_eq!(mock.sent_messages.lock().unwrap().len(), 1);
        assert_eq!(mock.sent_messages.lock().unwrap()[0].1, "> 🎤 hello");
        assert!(mock.reactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_echo_failure_adds_warning_reaction() {
        let mock = Arc::new(MockAdapter::default());
        let adapter: Arc<dyn ChatAdapter> = mock.clone();
        let entries = vec![EchoEntry::Failed];
        post_echo(&adapter, &test_channel(), &test_trigger(), &entries, &cfg(true)).await;

        assert_eq!(mock.sent_messages.lock().unwrap().len(), 1);
        assert_eq!(mock.sent_messages.lock().unwrap()[0].1, "> 🎤 (transcription failed)");
        let reactions = mock.reactions.lock().unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].1, "⚠️");
    }

    #[tokio::test]
    async fn post_echo_mixed_one_message_one_reaction() {
        let mock = Arc::new(MockAdapter::default());
        let adapter: Arc<dyn ChatAdapter> = mock.clone();
        let entries = vec![
            EchoEntry::Success("ok".into()),
            EchoEntry::Failed,
        ];
        post_echo(&adapter, &test_channel(), &test_trigger(), &entries, &cfg(true)).await;

        assert_eq!(mock.sent_messages.lock().unwrap().len(), 1);
        assert_eq!(mock.sent_messages.lock().unwrap()[0].1, "> 🎤 ok\n> 🎤 (transcription failed)");
        assert_eq!(mock.reactions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn post_echo_disabled_is_noop() {
        let mock = Arc::new(MockAdapter::default());
        let adapter: Arc<dyn ChatAdapter> = mock.clone();
        let entries = vec![EchoEntry::Success("hi".into()), EchoEntry::Failed];
        post_echo(&adapter, &test_channel(), &test_trigger(), &entries, &cfg(false)).await;

        assert!(mock.sent_messages.lock().unwrap().is_empty());
        assert!(mock.reactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_echo_empty_entries_is_noop() {
        let mock = Arc::new(MockAdapter::default());
        let adapter: Arc<dyn ChatAdapter> = mock.clone();
        let entries: Vec<EchoEntry> = vec![];
        post_echo(&adapter, &test_channel(), &test_trigger(), &entries, &cfg(true)).await;

        assert!(mock.sent_messages.lock().unwrap().is_empty());
        assert!(mock.reactions.lock().unwrap().is_empty());
    }
}

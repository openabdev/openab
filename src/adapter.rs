use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use tracing::error;

use crate::acp::{classify_notification, AcpEvent, ContentBlock, SessionPool};
use crate::config::ReactionsConfig;
use crate::error_display::{format_coded_error, format_user_error};
use crate::format;
use crate::reactions::StatusReactionController;

// --- Platform-agnostic types ---

/// Identifies a channel or thread across platforms.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct ChannelRef {
    pub platform: String,
    pub channel_id: String,
    /// Thread within a channel (e.g. Slack thread_ts, Telegram topic_id).
    /// For Discord, threads are separate channels so this is None.
    pub thread_id: Option<String>,
    /// Parent channel if this is a thread-as-channel (Discord).
    pub parent_id: Option<String>,
}

/// Identifies a message across platforms.
#[derive(Clone, Debug)]
pub struct MessageRef {
    pub channel: ChannelRef,
    pub message_id: String,
}

/// Sender identity injected into prompts for downstream agent context.
#[derive(Clone, Debug, Serialize)]
pub struct SenderContext {
    pub schema: String,
    pub sender_id: String,
    pub sender_name: String,
    pub display_name: String,
    pub channel: String,
    pub channel_id: String,
    /// Thread identifier, if the message is inside a thread.
    /// Slack: thread_ts. Discord: None (threads are separate channels).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub is_bot: bool,
}

// --- ChatAdapter trait ---

#[async_trait]
pub trait ChatAdapter: Send + Sync + 'static {
    /// Platform name for logging and session key namespacing.
    fn platform(&self) -> &'static str;

    /// Maximum message length for this platform (e.g. 2000 for Discord, 4000 for Slack).
    fn message_limit(&self) -> usize;

    /// Send a new message, returns a reference to the sent message.
    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef>;

    /// Create a thread from a trigger message, returns the thread channel ref.
    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef>;

    /// Add a reaction/emoji to a message.
    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;

    /// Remove a reaction/emoji from a message.
    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;

    /// Edit an existing message in-place (for streaming updates).
    /// Default: unsupported (send-once only).
    async fn edit_message(&self, _msg: &MessageRef, _content: &str) -> Result<()> {
        Err(anyhow::anyhow!("edit_message not supported"))
    }

    /// Whether this adapter should use streaming edit (true) or send-once (false).
    fn use_streaming(&self) -> bool {
        false
    }
}

// --- AdapterRouter ---

/// Shared logic for routing messages to ACP agents, managing sessions,
/// streaming edits, and controlling reactions. Platform-independent.
pub struct AdapterRouter {
    pool: Arc<SessionPool>,
    reactions_config: ReactionsConfig,
}

impl AdapterRouter {
    pub fn new(pool: Arc<SessionPool>, reactions_config: ReactionsConfig) -> Self {
        Self {
            pool,
            reactions_config,
        }
    }

    /// Access the underlying session pool (e.g. for config option queries).
    pub fn pool(&self) -> &Arc<SessionPool> {
        &self.pool
    }

    /// Handle an incoming user message. The adapter is responsible for
    /// filtering, resolving the thread, and building the SenderContext.
    /// This method handles sender context injection, session management, and streaming.
    pub async fn handle_message(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        thread_channel: &ChannelRef,
        sender_json: &str,
        prompt: &str,
        extra_blocks: Vec<ContentBlock>,
        trigger_msg: &MessageRef,
    ) -> Result<()> {
        tracing::debug!(platform = adapter.platform(), "processing message");

        // Build content blocks: sender context + prompt text, then extra (images, transcripts)
        let prompt_with_sender = format!(
            "<sender_context>\n{}\n</sender_context>\n\n{}",
            sender_json, prompt
        );

        let mut content_blocks = Vec::with_capacity(1 + extra_blocks.len());
        // Prepend any transcript blocks (they go before the text block)
        for block in &extra_blocks {
            if matches!(block, ContentBlock::Text { .. }) {
                content_blocks.push(block.clone());
            }
        }
        content_blocks.push(ContentBlock::Text {
            text: prompt_with_sender,
        });
        // Append non-text blocks (images)
        for block in extra_blocks {
            if !matches!(block, ContentBlock::Text { .. }) {
                content_blocks.push(block);
            }
        }

        let thread_key = format!(
            "{}:{}",
            adapter.platform(),
            thread_channel
                .thread_id
                .as_deref()
                .unwrap_or(&thread_channel.channel_id)
        );

        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let msg = format_user_error(&e.to_string());
            let _ = adapter
                .send_message(thread_channel, &format!("⚠️ {msg}"))
                .await;
            error!("pool error: {e}");
            return Err(e);
        }

        let reactions = Arc::new(StatusReactionController::new(
            self.reactions_config.enabled,
            adapter.clone(),
            trigger_msg.clone(),
            self.reactions_config.emojis.clone(),
            self.reactions_config.timing.clone(),
        ));
        reactions.set_queued().await;

        let result = self
            .stream_prompt(
                adapter,
                &thread_key,
                content_blocks,
                thread_channel,
                reactions.clone(),
            )
            .await;

        match &result {
            Ok(()) => reactions.set_done().await,
            Err(_) => reactions.set_error().await,
        }

        let hold_ms = if result.is_ok() {
            self.reactions_config.timing.done_hold_ms
        } else {
            self.reactions_config.timing.error_hold_ms
        };
        if self.reactions_config.remove_after_reply {
            let reactions = reactions;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
                reactions.clear().await;
            });
        }

        if let Err(ref e) = result {
            let _ = adapter
                .send_message(thread_channel, &format!("⚠️ {e}"))
                .await;
        }

        result
    }

    async fn stream_prompt(
        &self,
        adapter: &Arc<dyn ChatAdapter>,
        thread_key: &str,
        content_blocks: Vec<ContentBlock>,
        thread_channel: &ChannelRef,
        reactions: Arc<StatusReactionController>,
    ) -> Result<()> {
        let adapter = adapter.clone();
        let thread_channel = thread_channel.clone();
        let message_limit = adapter.message_limit();
        let streaming = adapter.use_streaming();

        self.pool
            .with_connection(thread_key, |conn| {
                let content_blocks = content_blocks.clone();
                Box::pin(async move {
                    let reset = conn.session_reset;
                    conn.session_reset = false;

                    let (mut rx, _) = conn.session_prompt(content_blocks).await?;
                    reactions.set_thinking().await;

                    let mut text_buf = String::new();
                    let mut tool_lines: Vec<ToolEntry> = Vec::new();

                    if reset {
                        text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
                    }

                    // Streaming edit: send placeholder, spawn edit loop
                    let (buf_tx, placeholder_msg) = if streaming {
                        let initial = if reset {
                            "⚠️ _Session expired, starting fresh..._\n\n…".to_string()
                        } else {
                            "…".to_string()
                        };
                        let msg = adapter.send_message(&thread_channel, &initial).await?;
                        let (tx, rx) = tokio::sync::watch::channel(initial);
                        let edit_adapter = adapter.clone();
                        let edit_msg = msg.clone();
                        let limit = message_limit;
                        let mut buf_rx = rx;
                        tokio::spawn(async move {
                            let mut last = String::new();
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                                if buf_rx.has_changed().unwrap_or(false) {
                                    let content = buf_rx.borrow_and_update().clone();
                                    if content != last {
                                        let display = if content.chars().count() > limit - 100 {
                                            format!("…{}", format::truncate_chars_tail(&content, limit - 100))
                                        } else {
                                            content.clone()
                                        };
                                        let _ = edit_adapter.edit_message(&edit_msg, &display).await;
                                        last = content;
                                    }
                                }
                                if buf_rx.has_changed().is_err() { break; }
                            }
                        });
                        (Some(tx), Some(msg))
                    } else {
                        (None, None)
                    };

                    // Process ACP notifications
                    let mut response_error: Option<String> = None;
                    let recv_timeout = std::time::Duration::from_secs(600);
                    loop {
                        let notification = match tokio::time::timeout(recv_timeout, rx.recv()).await {
                            Ok(Some(n)) => n,
                            Ok(None) => break, // channel closed
                            Err(_) => {
                                response_error = Some("Agent stopped responding".into());
                                break;
                            }
                        };
                        if notification.id.is_some() {
                            if let Some(ref err) = notification.error {
                                response_error = Some(format_coded_error(err.code, &err.message));
                            }
                            break;
                        }

                        if let Some(event) = classify_notification(&notification) {
                            match event {
                                AcpEvent::Text(t) => {
                                    text_buf.push_str(&t);
                                    if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(&tool_lines, &text_buf, true));
                                    }
                                }
                                AcpEvent::Thinking => {
                                    reactions.set_thinking().await;
                                }
                                AcpEvent::ToolStart { id, title } if !title.is_empty() => {
                                    reactions.set_tool(&title).await;
                                    let title = sanitize_title(&title);
                                    if let Some(slot) = tool_lines.iter_mut().find(|e| e.id == id) {
                                        slot.title = title;
                                        slot.state = ToolState::Running;
                                    } else {
                                        tool_lines.push(ToolEntry {
                                            id,
                                            title,
                                            state: ToolState::Running,
                                        });
                                    }
                                    if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(&tool_lines, &text_buf, true));
                                    }
                                }
                                AcpEvent::ToolDone { id, title, status } => {
                                    reactions.set_thinking().await;
                                    let new_state = if status == "completed" {
                                        ToolState::Completed
                                    } else {
                                        ToolState::Failed
                                    };
                                    if let Some(slot) = tool_lines.iter_mut().find(|e| e.id == id) {
                                        if !title.is_empty() {
                                            slot.title = sanitize_title(&title);
                                        }
                                        slot.state = new_state;
                                    } else if !title.is_empty() {
                                        tool_lines.push(ToolEntry {
                                            id,
                                            title: sanitize_title(&title),
                                            state: new_state,
                                        });
                                    }
                                    if let Some(tx) = &buf_tx {
                                        let _ = tx.send(compose_display(&tool_lines, &text_buf, true));
                                    }
                                }
                                AcpEvent::ConfigUpdate { options } => {
                                    conn.config_options = options;
                                }
                                _ => {}
                            }
                        }
                    }

                    conn.prompt_done().await;
                    // Stop the edit loop
                    drop(buf_tx);

                    // Build final content
                    let final_content = compose_display(&tool_lines, &text_buf, false);
                    let final_content = if final_content.is_empty() {
                        if let Some(err) = response_error {
                            format!("⚠️ {err}")
                        } else {
                            "_(no response)_".to_string()
                        }
                    } else if let Some(err) = response_error {
                        format!("⚠️ {err}\n\n{final_content}")
                    } else {
                        final_content
                    };

                    let chunks = format::split_message(&final_content, message_limit);
                    if let Some(msg) = placeholder_msg {
                        // Streaming: edit first chunk into placeholder, send rest as new messages
                        if let Some(first) = chunks.first() {
                            let _ = adapter.edit_message(&msg, first).await;
                        }
                        for chunk in chunks.iter().skip(1) {
                            let _ = adapter.send_message(&thread_channel, chunk).await;
                        }
                    } else {
                        // Send-once: all chunks as new messages
                        for chunk in &chunks {
                            let _ = adapter.send_message(&thread_channel, chunk).await;
                        }
                    }

                    Ok(())
                })
            })
            .await
    }
}

/// Flatten a tool-call title into a single line safe for inline-code spans.
fn sanitize_title(title: &str) -> String {
    title.replace('\r', "").replace('\n', " ; ").replace('`', "'")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
struct ToolEntry {
    id: String,
    title: String,
    state: ToolState,
}

impl ToolEntry {
    fn render(&self) -> String {
        let icon = match self.state {
            ToolState::Running => "🔧",
            ToolState::Completed => "✅",
            ToolState::Failed => "❌",
        };
        let suffix = if self.state == ToolState::Running { "..." } else { "" };
        format!("{icon} `{}`{}", self.title, suffix)
    }
}

/// Maximum number of finished tool entries to show individually
/// during streaming before collapsing into a summary line.
const TOOL_COLLAPSE_THRESHOLD: usize = 3;

fn compose_display(tool_lines: &[ToolEntry], text: &str, streaming: bool) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() {
        if streaming {
            let done = tool_lines.iter().filter(|e| e.state == ToolState::Completed).count();
            let failed = tool_lines.iter().filter(|e| e.state == ToolState::Failed).count();
            let running: Vec<_> = tool_lines.iter().filter(|e| e.state == ToolState::Running).collect();
            let finished = done + failed;

            if finished <= TOOL_COLLAPSE_THRESHOLD {
                for entry in tool_lines.iter().filter(|e| e.state != ToolState::Running) {
                    out.push_str(&entry.render());
                    out.push('\n');
                }
            } else {
                let mut parts = Vec::new();
                if done > 0 { parts.push(format!("✅ {done}")); }
                if failed > 0 { parts.push(format!("❌ {failed}")); }
                out.push_str(&format!("{} tool(s) completed\n", parts.join(" · ")));
            }

            if running.len() <= TOOL_COLLAPSE_THRESHOLD {
                for entry in &running {
                    out.push_str(&entry.render());
                    out.push('\n');
                }
            } else {
                let hidden = running.len() - TOOL_COLLAPSE_THRESHOLD;
                out.push_str(&format!("🔧 {hidden} more running\n"));
                for entry in running.iter().skip(hidden) {
                    out.push_str(&entry.render());
                    out.push('\n');
                }
            }
        } else {
            for entry in tool_lines {
                out.push_str(&entry.render());
                out.push('\n');
            }
        }
        if !out.is_empty() { out.push('\n'); }
    }
    out.push_str(text.trim_end());
    out
}

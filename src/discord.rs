use crate::acp::{classify_notification, AcpEvent, SessionPool};
use crate::config::{MultiAgentConfig, ReactionsConfig};
use crate::format;
use crate::reactions::StatusReactionController;
use serenity::async_trait;
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, MessageId};
use serenity::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{watch, RwLock};
use tracing::{error, info};

/// Tracks per-thread turn counts and cooldowns for multi-agent loop prevention.
pub struct ThreadTracker {
    /// thread_key -> (turn_count, last_response_time)
    threads: HashMap<String, (u32, Instant)>,
}

impl ThreadTracker {
    pub fn new() -> Self {
        Self { threads: HashMap::new() }
    }

    pub fn should_respond(&self, thread_key: &str, config: &MultiAgentConfig) -> bool {
        if let Some((turns, last_time)) = self.threads.get(thread_key) {
            if *turns >= config.max_ping_pong_turns {
                return false;
            }
            if last_time.elapsed().as_secs() < config.cooldown_secs {
                return false;
            }
        }
        true
    }

    pub fn record(&mut self, thread_key: &str) {
        let entry = self.threads.entry(thread_key.to_string()).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
    }

    #[allow(dead_code)]
    pub fn reset(&mut self, thread_key: &str) {
        self.threads.remove(thread_key);
    }
}

/// Agent response content after processing, used to detect REPLY_SKIP.
const REPLY_SKIP: &str = "REPLY_SKIP";

pub struct Handler {
    pub pool: Arc<SessionPool>,
    pub allowed_channels: HashSet<u64>,
    pub allowed_users: HashSet<u64>,
    pub reactions_config: ReactionsConfig,
    pub respond_without_mention: bool,
    pub multi_agent: MultiAgentConfig,
    pub thread_tracker: Arc<RwLock<ThreadTracker>>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let bot_id = ctx.cache.current_user().id;

        let channel_id = msg.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);

        let is_mentioned = msg.mentions_user_id(bot_id)
            || msg.content.contains(&format!("<@{}>", bot_id))
            || msg.mention_roles.iter().any(|r| msg.content.contains(&format!("<@&{}>", r)));

        let in_thread = if !in_allowed_channel {
            match msg.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => {
                    let result = gc
                        .parent_id
                        .map_or(false, |pid| self.allowed_channels.contains(&pid.get()));
                    tracing::debug!(channel_id = %msg.channel_id, parent_id = ?gc.parent_id, result, "thread check");
                    result
                }
                Ok(other) => {
                    tracing::debug!(channel_id = %msg.channel_id, kind = ?other, "not a guild channel");
                    false
                }
                Err(e) => {
                    tracing::debug!(channel_id = %msg.channel_id, error = %e, "to_channel failed");
                    false
                }
            }
        } else {
            false
        };

        if !in_allowed_channel && !in_thread {
            return;
        }
        if !in_thread && !is_mentioned && !self.respond_without_mention {
            return;
        }

        // Multi-agent: check thread turn limit and cooldown
        if in_thread {
            let thread_key = msg.channel_id.get().to_string();
            let tracker = self.thread_tracker.read().await;
            if !tracker.should_respond(&thread_key, &self.multi_agent) {
                tracing::debug!(thread = %thread_key, "blocked by turn limit or cooldown");
                return;
            }
        }

        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&msg.author.id.get()) {
            tracing::info!(user_id = %msg.author.id, "denied user, ignoring");
            if let Err(e) = msg.react(&ctx.http, ReactionType::Unicode("🚫".into())).await {
                tracing::warn!(error = %e, "failed to react with 🚫");
            }
            return;
        }

        let prompt = if is_mentioned {
            strip_mention(&msg.content)
        } else {
            msg.content.trim().to_string()
        };
        if prompt.is_empty() {
            return;
        }

        // Inject structured sender context so the downstream CLI can identify who sent the message
        let display_name = msg.member.as_ref()
            .and_then(|m| m.nick.as_ref())
            .unwrap_or(&msg.author.name);
        let sender_ctx = serde_json::json!({
            "schema": "openab.sender.v1",
            "sender_id": msg.author.id.to_string(),
            "sender_name": msg.author.name,
            "display_name": display_name,
            "channel": "discord",
            "channel_id": msg.channel_id.to_string(),
            "is_bot": msg.author.bot,
        });
        let prompt_with_sender = format!(
            "<sender_context>\n{}\n</sender_context>\n\n{}",
            serde_json::to_string(&sender_ctx).unwrap(),
            prompt
        );

        tracing::debug!(prompt = %prompt_with_sender, in_thread, "processing");

        let thread_id = if in_thread {
            msg.channel_id.get()
        } else {
            match get_or_create_thread(&ctx, &msg, &prompt).await {
                Ok(id) => id,
                Err(e) => {
                    error!("failed to create thread: {e}");
                    return;
                }
            }
        };

        let thread_channel = ChannelId::new(thread_id);

        let thinking_msg = match thread_channel.say(&ctx.http, "...").await {
            Ok(m) => m,
            Err(e) => {
                error!("failed to post: {e}");
                return;
            }
        };

        let thread_key = thread_id.to_string();
        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let _ = edit(&ctx, thread_channel, thinking_msg.id, "⚠️ Failed to start agent.").await;
            error!("pool error: {e}");
            return;
        }

        // Create reaction controller on the user's original message
        let reactions = Arc::new(StatusReactionController::new(
            self.reactions_config.enabled,
            ctx.http.clone(),
            msg.channel_id,
            msg.id,
            self.reactions_config.emojis.clone(),
            self.reactions_config.timing.clone(),
        ));
        reactions.set_queued().await;

        // Stream prompt with live edits
        let result = stream_prompt(
            &self.pool,
            &thread_key,
            &prompt_with_sender,
            &ctx,
            thread_channel,
            thinking_msg.id,
            reactions.clone(),
        )
        .await;

        // Multi-agent: track turns and detect REPLY_SKIP
        match &result {
            Ok(response_text) => {
                if response_text.trim() == REPLY_SKIP {
                    // Agent voluntarily skipped — delete the placeholder message
                    let _ = thread_channel.delete_message(&ctx.http, thinking_msg.id).await;
                    tracing::info!(thread = %thread_key, "agent replied REPLY_SKIP, suppressing");
                } else {
                    // Record the turn
                    let mut tracker = self.thread_tracker.write().await;
                    tracker.record(&thread_key);
                }
                reactions.set_done().await;
            }
            Err(_) => reactions.set_error().await,
        }

        // Hold emoji briefly then clear
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

        if let Err(e) = result {
            let _ = edit(&ctx, thread_channel, thinking_msg.id, &format!("⚠️ {e}")).await;
        }
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "discord bot connected");
    }
}

async fn edit(ctx: &Context, ch: ChannelId, msg_id: MessageId, content: &str) -> serenity::Result<Message> {
    ch.edit_message(&ctx.http, msg_id, serenity::builder::EditMessage::new().content(content)).await
}

async fn stream_prompt(
    pool: &SessionPool,
    thread_key: &str,
    prompt: &str,
    ctx: &Context,
    channel: ChannelId,
    msg_id: MessageId,
    reactions: Arc<StatusReactionController>,
) -> anyhow::Result<String> {
    let prompt = prompt.to_string();
    let reactions = reactions.clone();

    pool.with_connection(thread_key, |conn| {
        let prompt = prompt.clone();
        let ctx = ctx.clone();
        let reactions = reactions.clone();
        Box::pin(async move {
            let reset = conn.session_reset;
            conn.session_reset = false;

            let (mut rx, _) = conn.session_prompt(&prompt).await?;
            reactions.set_thinking().await;

            let initial = if reset {
                "⚠️ _Session expired, starting fresh..._\n\n...".to_string()
            } else {
                "...".to_string()
            };
            let (buf_tx, buf_rx) = watch::channel(initial);

            let mut text_buf = String::new();
            let mut tool_lines: Vec<String> = Vec::new();
            let current_msg_id = msg_id;

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            // Spawn edit-streaming task
            let edit_handle = {
                let ctx = ctx.clone();
                let mut buf_rx = buf_rx.clone();
                tokio::spawn(async move {
                    let mut last_content = String::new();
                    let mut current_edit_msg = msg_id;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        if buf_rx.has_changed().unwrap_or(false) {
                            let content = buf_rx.borrow_and_update().clone();
                            if content != last_content {
                                if content.len() > 1900 {
                                    let chunks = format::split_message(&content, 1900);
                                    if let Some(first) = chunks.first() {
                                        let _ = edit(&ctx, channel, current_edit_msg, first).await;
                                    }
                                    for chunk in chunks.iter().skip(1) {
                                        if let Ok(new_msg) = channel.say(&ctx.http, chunk).await {
                                            current_edit_msg = new_msg.id;
                                        }
                                    }
                                } else {
                                    let _ = edit(&ctx, channel, current_edit_msg, &content).await;
                                }
                                last_content = content;
                            }
                        }
                        if buf_rx.has_changed().is_err() {
                            break;
                        }
                    }
                })
            };

            // Process ACP notifications
            let mut got_first_text = false;
            while let Some(notification) = rx.recv().await {
                if notification.id.is_some() {
                    break;
                }

                if let Some(event) = classify_notification(&notification) {
                    match event {
                        AcpEvent::Text(t) => {
                            if !got_first_text {
                                got_first_text = true;
                                // Reaction: back to thinking after tools
                            }
                            text_buf.push_str(&t);
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        AcpEvent::Thinking => {
                            reactions.set_thinking().await;
                        }
                        AcpEvent::ToolStart { title, .. } if !title.is_empty() => {
                            reactions.set_tool(&title).await;
                            tool_lines.push(format!("🔧 `{title}`..."));
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        AcpEvent::ToolDone { title, status, .. } => {
                            reactions.set_thinking().await;
                            let icon = if status == "completed" { "✅" } else { "❌" };
                            if let Some(line) = tool_lines.iter_mut().rev().find(|l| l.contains(&title)) {
                                *line = format!("{icon} `{title}`");
                            }
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        _ => {}
                    }
                }
            }

            conn.prompt_done().await;
            drop(buf_tx);
            let _ = edit_handle.await;

            // Final edit
            let final_content = compose_display(&tool_lines, &text_buf);
            let final_content = if final_content.is_empty() {
                "_(no response)_".to_string()
            } else {
                final_content
            };

            let chunks = format::split_message(&final_content, 2000);
            for (i, chunk) in chunks.iter().enumerate() {
                if i == 0 {
                    let _ = edit(&ctx, channel, current_msg_id, chunk).await;
                } else {
                    let _ = channel.say(&ctx.http, chunk).await;
                }
            }

            Ok(text_buf)
        })
    })
    .await
}

fn compose_display(tool_lines: &[String], text: &str) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() {
        for line in tool_lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(text.trim_end());
    out
}

fn strip_mention(content: &str) -> String {
    let re = regex::Regex::new(r"<@[!&]?\d+>").unwrap();
    re.replace_all(content, "").trim().to_string()
}

fn shorten_thread_name(prompt: &str) -> String {
    // Shorten GitHub URLs: https://github.com/owner/repo/issues/123 → owner/repo#123
    let re = regex::Regex::new(r"https?://github\.com/([^/]+/[^/]+)/(issues|pull)/(\d+)").unwrap();
    let shortened = re.replace_all(prompt, "$1#$3");
    let name: String = shortened.chars().take(40).collect();
    if name.len() < shortened.len() {
        format!("{name}...")
    } else {
        name
    }
}

async fn get_or_create_thread(ctx: &Context, msg: &Message, prompt: &str) -> anyhow::Result<u64> {
    let channel = msg.channel_id.to_channel(&ctx.http).await?;
    if let serenity::model::channel::Channel::Guild(ref gc) = channel {
        if gc.thread_metadata.is_some() {
            return Ok(msg.channel_id.get());
        }
    }

    let thread_name = shorten_thread_name(prompt);

    let thread = msg
        .channel_id
        .create_thread_from_message(
            &ctx.http,
            msg.id,
            serenity::builder::CreateThread::new(thread_name)
                .auto_archive_duration(serenity::model::channel::AutoArchiveDuration::OneDay),
        )
        .await?;

    Ok(thread.id.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MultiAgentConfig;

    fn test_config(max_turns: u32, cooldown: u64) -> MultiAgentConfig {
        MultiAgentConfig {
            allow_delegation: true,
            max_ping_pong_turns: max_turns,
            cooldown_secs: cooldown,
        }
    }

    // -----------------------------------------------------------------------
    // ThreadTracker: max_ping_pong_turns (inspired by CrewAI max_iter tests)
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_thread_always_allowed() {
        let tracker = ThreadTracker::new();
        let config = test_config(5, 0);
        assert!(tracker.should_respond("thread-1", &config));
    }

    #[test]
    fn test_max_turns_blocks_after_limit() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(3, 0);

        // Record 3 turns
        for _ in 0..3 {
            assert!(tracker.should_respond("thread-1", &config));
            tracker.record("thread-1");
        }

        // 4th should be blocked
        assert!(!tracker.should_respond("thread-1", &config));
    }

    #[test]
    fn test_zero_turns_fire_and_forget() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(0, 0); // fire-and-forget

        // First message in a new thread is allowed (no entry yet)
        assert!(tracker.should_respond("thread-1", &config));

        // After one record, should block (0 >= 0)
        tracker.record("thread-1");
        assert!(!tracker.should_respond("thread-1", &config));
    }

    #[test]
    fn test_different_threads_independent() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(2, 0);

        tracker.record("thread-a");
        tracker.record("thread-a");

        // thread-a blocked
        assert!(!tracker.should_respond("thread-a", &config));
        // thread-b still allowed
        assert!(tracker.should_respond("thread-b", &config));
    }

    // -----------------------------------------------------------------------
    // ThreadTracker: human reset (inspired by CrewAI delegation reset)
    // -----------------------------------------------------------------------

    #[test]
    fn test_reset_clears_turn_count() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(2, 0);

        tracker.record("thread-1");
        tracker.record("thread-1");
        assert!(!tracker.should_respond("thread-1", &config));

        // Human intervenes — reset
        tracker.reset("thread-1");
        assert!(tracker.should_respond("thread-1", &config));
    }

    #[test]
    fn test_reset_nonexistent_thread_is_noop() {
        let mut tracker = ThreadTracker::new();
        tracker.reset("does-not-exist"); // should not panic
    }

    // -----------------------------------------------------------------------
    // ThreadTracker: cooldown
    // -----------------------------------------------------------------------

    #[test]
    fn test_cooldown_blocks_rapid_responses() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(10, 60); // 60 second cooldown

        tracker.record("thread-1");

        // Immediately after — should be blocked by cooldown
        assert!(!tracker.should_respond("thread-1", &config));
    }

    #[test]
    fn test_no_cooldown_allows_immediate() {
        let mut tracker = ThreadTracker::new();
        let config = test_config(10, 0); // no cooldown

        tracker.record("thread-1");
        assert!(tracker.should_respond("thread-1", &config));
    }

    // -----------------------------------------------------------------------
    // REPLY_SKIP detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_reply_skip_exact_match() {
        assert_eq!("REPLY_SKIP".trim(), REPLY_SKIP);
    }

    #[test]
    fn test_reply_skip_with_whitespace() {
        assert_eq!("  REPLY_SKIP  ".trim(), REPLY_SKIP);
    }

    #[test]
    fn test_reply_skip_not_substring() {
        assert_ne!("I will REPLY_SKIP this".trim(), REPLY_SKIP);
    }

    #[test]
    fn test_normal_response_not_skip() {
        assert_ne!("這是正常的回覆".trim(), REPLY_SKIP);
    }

    // -----------------------------------------------------------------------
    // MultiAgentConfig defaults
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_config() {
        let config = MultiAgentConfig::default();
        assert!(config.allow_delegation);
        assert_eq!(config.max_ping_pong_turns, 5);
        assert_eq!(config.cooldown_secs, 10);
    }

    // -----------------------------------------------------------------------
    // Config parsing (toml)
    // -----------------------------------------------------------------------

    #[test]
    fn test_config_omitted_uses_defaults() {
        let toml_str = "";
        let config: MultiAgentConfig = toml::from_str(toml_str).unwrap_or_default();
        assert!(config.allow_delegation);
        assert_eq!(config.max_ping_pong_turns, 5);
    }

    #[test]
    fn test_config_leaf_agent() {
        let toml_str = r#"
            allow_delegation = false
            max_ping_pong_turns = 1
            cooldown_secs = 0
        "#;
        let config: MultiAgentConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.allow_delegation);
        assert_eq!(config.max_ping_pong_turns, 1);
        assert_eq!(config.cooldown_secs, 0);
    }

    #[test]
    fn test_config_fire_and_forget() {
        let toml_str = r#"
            max_ping_pong_turns = 0
        "#;
        let config: MultiAgentConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.max_ping_pong_turns, 0);
    }
}

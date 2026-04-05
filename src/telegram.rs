use crate::acp::{classify_notification, AcpEvent, SessionPool};
use crate::format;
use std::collections::HashSet;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    ChatKind, MessageId, PublicChatKind, ReactionType, ReplyParameters, ThreadId,
};
use tracing::{error, info};

// ── Emoji constants ──────────────────────────────────────────────────────────
const EMOJI_QUEUED: &str = "👀";
const EMOJI_THINKING: &str = "🤔";
const EMOJI_TOOL: &str = "🔥";
const EMOJI_CODING: &str = "👨‍💻";
const EMOJI_WEB: &str = "⚡";
const EMOJI_DONE: &str = "👍";
const EMOJI_ERROR: &str = "😱";

const CODING_TOKENS: &[&str] = &["exec", "process", "read", "write", "edit", "bash", "shell"];
const WEB_TOKENS: &[&str] = &["web_search", "web_fetch", "web-search", "web-fetch", "browser"];

fn tool_emoji(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if WEB_TOKENS.iter().any(|t| n.contains(t)) { EMOJI_WEB }
    else if CODING_TOKENS.iter().any(|t| n.contains(t)) { EMOJI_CODING }
    else { EMOJI_TOOL }
}

// ── Reaction helpers ─────────────────────────────────────────────────────────

async fn set_reaction(bot: &Bot, chat_id: ChatId, msg_id: MessageId, emoji: &str) {
    let _ = bot.set_message_reaction(chat_id, msg_id)
        .reaction(vec![ReactionType::Emoji { emoji: emoji.to_string() }])
        .await;
}

async fn clear_reaction(bot: &Bot, chat_id: ChatId, msg_id: MessageId) {
    let _ = bot.set_message_reaction(chat_id, msg_id)
        .reaction(vec![])
        .await;
}

// ── Thread context ───────────────────────────────────────────────────────────

/// Represents where the bot should send/edit messages for a conversation.
#[derive(Clone)]
struct ThreadCtx {
    chat_id: ChatId,
    /// The forum topic thread ID (if using forum topics).
    thread_id: Option<ThreadId>,
    /// Session key for the ACP pool.
    session_key: String,
}

/// Determine the thread context for an incoming message.
/// 
/// Strategy:
/// - If the chat is a forum supergroup (`is_forum`): use/create a forum topic per user.
/// - If it's a private chat with forum topics enabled: use/create a topic per conversation.
/// - Otherwise: use reply chains, session key = chat_id (DM) or chat_id:user_id (group).
async fn get_or_create_thread(bot: &Bot, msg: &Message) -> anyhow::Result<ThreadCtx> {
    let chat_id = msg.chat.id;
    let is_forum = matches!(
        &msg.chat.kind,
        ChatKind::Public(p) if matches!(&p.kind, PublicChatKind::Supergroup(s) if s.is_forum)
    );

    if is_forum {
        // Group forum or private chat with topics enabled.
        // If the message is already inside a topic, reuse it.
        if let Some(thread_id) = msg.thread_id {
            return Ok(ThreadCtx {
                chat_id,
                thread_id: Some(thread_id),
                session_key: format!("{}:{}", chat_id, thread_id),
            });
        }

        // Create a new topic named after the user + first words of prompt.
        let user_name = msg.from()
            .map(|u| u.first_name.clone())
            .unwrap_or_else(|| "User".to_string());
        let prompt_preview: String = msg.text().unwrap_or("").chars().take(30).collect();
        let topic_name = format!("{}: {}", user_name, prompt_preview);
        let topic_name: String = topic_name.chars().take(128).collect();

        let topic = bot.create_forum_topic(chat_id, topic_name, 0x6FB9F0u32, "")
            .await?;
        let thread_id = topic.thread_id;
        Ok(ThreadCtx {
            chat_id,
            thread_id: Some(thread_id),
            session_key: format!("{}:{}", chat_id, thread_id),
        })
    } else {
        // Plain DM or regular group — use reply chains.
        // Session key: chat_id for DMs, chat_id:user_id for groups.
        let session_key = if msg.chat.is_private() {
            chat_id.to_string()
        } else {
            let user_id = msg.from().map(|u| u.id.0).unwrap_or(0);
            format!("{}:{}", chat_id, user_id)
        };
        Ok(ThreadCtx { chat_id, thread_id: None, session_key })
    }
}

// ── Main bot loop ────────────────────────────────────────────────────────────

pub async fn run(pool: Arc<SessionPool>, bot_token: String, allowed_users: HashSet<i64>) {
    let bot = Bot::new(bot_token);
    info!("telegram bot starting");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let allowed_users = allowed_users.clone();
        async move {
            // Auth check
            let user_id = msg.from().map(|u| u.id.0 as i64).unwrap_or(0);
            if !allowed_users.is_empty() && !allowed_users.contains(&user_id) {
                return Ok(());
            }

            let prompt = match msg.text() {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => return Ok(()),
            };

            let chat_id = msg.chat.id;
            let user_msg_id = msg.id;

            // 👀 queued reaction on user's message
            set_reaction(&bot, chat_id, user_msg_id, EMOJI_QUEUED).await;

            // Resolve thread context
            let ctx = match get_or_create_thread(&bot, &msg).await {
                Ok(c) => c,
                Err(e) => {
                    error!("thread setup failed: {e}");
                    clear_reaction(&bot, chat_id, user_msg_id).await;
                    return Ok(());
                }
            };

            // Send initial "..." in the right thread/topic
            let thinking = {
                let mut req = bot.send_message(chat_id, "...");
                if let Some(tid) = ctx.thread_id {
                    req = req.message_thread_id(tid);
                } else {
                    // Reply to user's message for visual threading
                    req = req.reply_parameters(ReplyParameters::new(user_msg_id));
                }
                match req.await {
                    Ok(m) => m,
                    Err(e) => {
                        error!("send error: {e}");
                        clear_reaction(&bot, chat_id, user_msg_id).await;
                        return Ok(());
                    }
                }
            };

            if let Err(e) = pool.get_or_create(&ctx.session_key).await {
                let _ = bot.edit_message_text(chat_id, thinking.id, "⚠️ Failed to start agent.").await;
                clear_reaction(&bot, chat_id, user_msg_id).await;
                error!("pool error: {e}");
                return Ok(());
            }

            set_reaction(&bot, chat_id, user_msg_id, EMOJI_THINKING).await;

            let result = stream_prompt(
                &pool, &ctx.session_key, &prompt,
                &bot, chat_id, thinking.id, user_msg_id, ctx.thread_id,
            ).await;

            match &result {
                Ok(()) => {
                    set_reaction(&bot, chat_id, user_msg_id, EMOJI_DONE).await;
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    clear_reaction(&bot, chat_id, user_msg_id).await;
                }
                Err(_) => {
                    set_reaction(&bot, chat_id, user_msg_id, EMOJI_ERROR).await;
                    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                    clear_reaction(&bot, chat_id, user_msg_id).await;
                }
            }

            if let Err(e) = result {
                let _ = bot.edit_message_text(chat_id, thinking.id, format!("⚠️ {e}")).await;
            }

            Ok(())
        }
    })
    .await;
}

// ── Streaming prompt with live edits ────────────────────────────────────────

async fn stream_prompt(
    pool: &SessionPool,
    session_key: &str,
    prompt: &str,
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    user_msg_id: MessageId,
    thread_id: Option<ThreadId>,
) -> anyhow::Result<()> {
    let prompt = prompt.to_string();
    let bot = bot.clone();

    pool.with_connection(session_key, |conn| {
        let prompt = prompt.clone();
        let bot = bot.clone();
        Box::pin(async move {
            let reset = conn.session_reset;
            conn.session_reset = false;

            let (mut rx, _) = conn.session_prompt(&prompt).await?;

            let mut text_buf = String::new();
            let mut tool_lines: Vec<String> = Vec::new();
            let mut last_sent = String::new();
            let mut current_msg_id = msg_id;
            let mut last_edit = tokio::time::Instant::now();

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            while let Some(notification) = rx.recv().await {
                if notification.id.is_some() { break; }

                if let Some(event) = classify_notification(&notification) {
                    match event {
                        AcpEvent::Text(t) => { text_buf.push_str(&t); }
                        AcpEvent::Thinking => {
                            set_reaction(&bot, chat_id, user_msg_id, EMOJI_THINKING).await;
                        }
                        AcpEvent::ToolStart { title, .. } if !title.is_empty() => {
                            set_reaction(&bot, chat_id, user_msg_id, tool_emoji(&title)).await;
                            tool_lines.push(format!("🔧 `{title}`..."));
                        }
                        AcpEvent::ToolDone { title, status, .. } => {
                            set_reaction(&bot, chat_id, user_msg_id, EMOJI_THINKING).await;
                            let icon = if status == "completed" { "✅" } else { "❌" };
                            if let Some(line) = tool_lines.iter_mut().rev().find(|l| l.contains(&title)) {
                                *line = format!("{icon} `{title}`");
                            }
                        }
                        _ => {}
                    }
                }

                if last_edit.elapsed().as_millis() >= 1500 {
                    let content = compose_display(&tool_lines, &text_buf);
                    if content != last_sent && !content.is_empty() {
                        current_msg_id = send_chunks(&bot, chat_id, current_msg_id, &content, thread_id).await;
                        last_sent = content;
                        last_edit = tokio::time::Instant::now();
                    }
                }
            }

            conn.prompt_done().await;

            let final_content = compose_display(&tool_lines, &text_buf);
            let final_content = if final_content.is_empty() { "_(no response)_".to_string() } else { final_content };
            send_chunks(&bot, chat_id, current_msg_id, &final_content, thread_id).await;

            Ok(())
        })
    })
    .await
}

async fn send_chunks(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    content: &str,
    thread_id: Option<ThreadId>,
) -> MessageId {
    let chunks = format::split_message(content, 4000);
    let mut current = msg_id;
    for (i, chunk) in chunks.iter().enumerate() {
        if i == 0 {
            let _ = bot.edit_message_text(chat_id, current, chunk).await;
        } else {
            let mut req = bot.send_message(chat_id, chunk);
            if let Some(tid) = thread_id {
                req = req.message_thread_id(tid);
            }
            if let Ok(m) = req.await {
                current = m.id;
            }
        }
    }
    current
}

fn compose_display(tool_lines: &[String], text: &str) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() {
        for line in tool_lines { out.push_str(line); out.push('\n'); }
        out.push('\n');
    }
    out.push_str(text.trim_end());
    out
}

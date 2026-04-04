use crate::acp::{classify_notification, AcpEvent, SessionPool};
use crate::format;
use std::collections::HashSet;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ReactionType};
use tracing::{error, info};

// ── Emoji constants (mirrors openclaw status-reactions) ──────────────────────
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

// ── Reaction helper ──────────────────────────────────────────────────────────

async fn set_reaction(bot: &Bot, chat_id: ChatId, msg_id: MessageId, emoji: &str) {
    let reaction = vec![ReactionType::Emoji { emoji: emoji.to_string() }];
    let _ = bot.set_message_reaction(chat_id, msg_id)
        .reaction(reaction)
        .await;
}

async fn clear_reaction(bot: &Bot, chat_id: ChatId, msg_id: MessageId) {
    let _ = bot.set_message_reaction(chat_id, msg_id)
        .reaction(vec![])
        .await;
}

// ── Main bot loop ────────────────────────────────────────────────────────────

pub async fn run(pool: Arc<SessionPool>, bot_token: String, allowed_users: HashSet<i64>) {
    let bot = Bot::new(bot_token);
    info!("telegram bot starting");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let allowed_users = allowed_users.clone();
        async move {
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
            let thread_key = chat_id.to_string();

            // 👀 queued
            set_reaction(&bot, chat_id, user_msg_id, EMOJI_QUEUED).await;

            let thinking = match bot.send_message(chat_id, "...").await {
                Ok(m) => m,
                Err(e) => { error!("send error: {e}"); return Ok(()); }
            };

            if let Err(e) = pool.get_or_create(&thread_key).await {
                let _ = bot.edit_message_text(chat_id, thinking.id, "⚠️ Failed to start agent.").await;
                clear_reaction(&bot, chat_id, user_msg_id).await;
                error!("pool error: {e}");
                return Ok(());
            }

            // 🤔 thinking
            set_reaction(&bot, chat_id, user_msg_id, EMOJI_THINKING).await;

            let result = stream_prompt(
                &pool, &thread_key, &prompt,
                &bot, chat_id, thinking.id, user_msg_id,
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
    thread_key: &str,
    prompt: &str,
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    user_msg_id: MessageId,
) -> anyhow::Result<()> {
    let prompt = prompt.to_string();
    let bot = bot.clone();

    pool.with_connection(thread_key, |conn| {
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
                        AcpEvent::Text(t) => {
                            text_buf.push_str(&t);
                        }
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

                // Throttle edits to every 1.5s
                if last_edit.elapsed().as_millis() >= 1500 {
                    let content = compose_display(&tool_lines, &text_buf);
                    if content != last_sent && !content.is_empty() {
                        current_msg_id = send_chunks(&bot, chat_id, current_msg_id, &content).await;
                        last_sent = content;
                        last_edit = tokio::time::Instant::now();
                    }
                }
            }

            conn.prompt_done().await;

            // Final edit
            let final_content = compose_display(&tool_lines, &text_buf);
            let final_content = if final_content.is_empty() { "_(no response)_".to_string() } else { final_content };
            send_chunks(&bot, chat_id, current_msg_id, &final_content).await;

            Ok(())
        })
    })
    .await
}

async fn send_chunks(bot: &Bot, chat_id: ChatId, msg_id: MessageId, content: &str) -> MessageId {
    let chunks = format::split_message(content, 4000);
    let mut current = msg_id;
    for (i, chunk) in chunks.iter().enumerate() {
        if i == 0 {
            let _ = bot.edit_message_text(chat_id, current, chunk).await;
        } else if let Ok(m) = bot.send_message(chat_id, chunk).await {
            current = m.id;
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

use crate::acp::{classify_notification, AcpEvent, SessionPool};
use crate::format;
use std::collections::HashSet;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::{error, info};

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
            let thread_key = chat_id.to_string();

            let thinking = match bot.send_message(chat_id, "...").await {
                Ok(m) => m,
                Err(e) => { error!("send error: {e}"); return Ok(()); }
            };

            if let Err(e) = pool.get_or_create(&thread_key).await {
                let _ = bot.edit_message_text(chat_id, thinking.id, "⚠️ Failed to start agent.").await;
                error!("pool error: {e}");
                return Ok(());
            }

            let result = stream_prompt(&pool, &thread_key, &prompt, &bot, chat_id, thinking.id).await;

            if let Err(e) = result {
                let _ = bot.edit_message_text(chat_id, thinking.id, format!("⚠️ {e}")).await;
            }

            Ok(())
        }
    })
    .await;
}

async fn stream_prompt(
    pool: &SessionPool,
    thread_key: &str,
    prompt: &str,
    bot: &Bot,
    chat_id: ChatId,
    msg_id: teloxide::types::MessageId,
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

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            let mut last_edit = tokio::time::Instant::now();

            while let Some(notification) = rx.recv().await {
                if notification.id.is_some() {
                    break;
                }

                if let Some(event) = classify_notification(&notification) {
                    match event {
                        AcpEvent::Text(t) => {
                            text_buf.push_str(&t);
                        }
                        AcpEvent::ToolStart { title, .. } if !title.is_empty() => {
                            tool_lines.push(format!("🔧 `{title}`..."));
                        }
                        AcpEvent::ToolDone { title, status, .. } => {
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
                        let chunks = format::split_message(&content, 4000);
                        if let Some(first) = chunks.first() {
                            let _ = bot.edit_message_text(chat_id, current_msg_id, first).await;
                        }
                        for chunk in chunks.iter().skip(1) {
                            if let Ok(new_msg) = bot.send_message(chat_id, chunk).await {
                                current_msg_id = new_msg.id;
                            }
                        }
                        last_sent = content;
                        last_edit = tokio::time::Instant::now();
                    }
                }
            }

            conn.prompt_done().await;

            // Final edit
            let final_content = compose_display(&tool_lines, &text_buf);
            let final_content = if final_content.is_empty() {
                "_(no response)_".to_string()
            } else {
                final_content
            };

            let chunks = format::split_message(&final_content, 4000);
            for (i, chunk) in chunks.iter().enumerate() {
                if i == 0 {
                    let _ = bot.edit_message_text(chat_id, current_msg_id, chunk).await;
                } else {
                    let _ = bot.send_message(chat_id, chunk).await;
                }
            }

            Ok(())
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

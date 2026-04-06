use crate::acp::{classify_notification, AcpEvent, SessionPool, SessionMeta};
use crate::config::ChatMode;
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
    /// The forum topic thread ID (if using forum topics).
    thread_id: Option<ThreadId>,
    /// Session key for the ACP pool.
    session_key: String,
    /// True if this topic was just created (so we should rename it after the response).
    is_new_topic: bool,
}

/// Determine the thread context for an incoming message.
///
/// Strategy:
/// - Forum supergroup, in a real topic (not #General): reuse that topic's session.
/// - Forum supergroup, in #General or no topic: only create a new topic if `is_kiro_cmd` is true.
/// - Otherwise (DM / plain group): use reply chains.
async fn get_or_create_thread(bot: &Bot, msg: &Message, is_kiro_cmd: bool) -> anyhow::Result<ThreadCtx> {
    let chat_id = msg.chat.id;
    let is_forum = matches!(
        &msg.chat.kind,
        ChatKind::Public(p) if matches!(&p.kind, PublicChatKind::Supergroup(s) if s.is_forum)
    );

    tracing::info!(chat_id = %chat_id, is_forum, thread_id = ?msg.thread_id, chat_kind = ?std::mem::discriminant(&msg.chat.kind), "incoming message");

    if is_forum {
        // thread_id=1 is the #General topic — treat it like no topic.
        let in_real_topic = msg.thread_id.map_or(false, |t| t.0 != MessageId(1));

        if in_real_topic {
            let thread_id = msg.thread_id.unwrap();
            return Ok(ThreadCtx {
                thread_id: Some(thread_id),
                session_key: format!("{}:{}", chat_id, thread_id),
                is_new_topic: false,
            });
        }

        // In #General / no topic: only spawn a new topic for `!kiro` commands.
        if !is_kiro_cmd {
            // For silent buffering or @mention replies, use a per-user session key
            // without creating a topic.
            let user_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);
            return Ok(ThreadCtx {
                thread_id: None,
                session_key: format!("{}:general:{}", chat_id, user_id),
                is_new_topic: false,
            });
        }

        let user_name = msg.from
            .as_ref()
            .map(|u| u.first_name.clone())
            .unwrap_or_else(|| "User".to_string());
        let prompt_preview: String = msg.text().unwrap_or("").chars().take(30).collect();
        let topic_name: String = format!("{}: {}", user_name, prompt_preview).chars().take(128).collect();

        let topic = bot.create_forum_topic(chat_id, topic_name, 0x6FB9F0u32, "").await?;
        let thread_id = topic.thread_id;
        Ok(ThreadCtx {
            thread_id: Some(thread_id),
            session_key: format!("{}:{}", chat_id, thread_id),
            is_new_topic: true,
        })
    } else {
        // Plain DM or regular group — use reply chains.
        let session_key = if msg.chat.is_private() {
            chat_id.to_string()
        } else {
            let user_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);
            format!("{}:{}", chat_id, user_id)
        };
        Ok(ThreadCtx { thread_id: None, session_key, is_new_topic: false })
    }
}

// ── Main bot loop ────────────────────────────────────────────────────────────

pub async fn run(pool: Arc<SessionPool>, bot_token: String, allowed_users: HashSet<i64>, topic_creator_id: Option<i64>, mode: ChatMode) {
    let bot = Bot::new(bot_token);
    info!("telegram bot starting");

    // Fetch bot's own username for @mention detection.
    let bot_username: Option<String> = bot.get_me().await.ok().map(|me| {
        me.username().to_lowercase()
    });
    info!(bot_username = ?bot_username, "bot identity resolved");

    // Wire eviction notifier so cleanup_idle can message users when their session expires.
    {
        let bot2 = bot.clone();
        let notifier: crate::acp::EvictNotifier = Arc::new(move |meta: SessionMeta| {
            let bot3 = bot2.clone();
            tokio::spawn(async move {
                let chat_id = ChatId(meta.chat_id);
                const MSG: &str =
                    "⏱ Your session was closed due to inactivity. Send any message to resume.";
                let sent = if let Some(tid) = meta.thread_id {
                    bot3.send_message(chat_id, MSG)
                        .message_thread_id(ThreadId(MessageId(tid)))
                        .await
                } else {
                    bot3.send_message(chat_id, MSG).await
                };
                // If the topic was deleted, fall back to the main chat.
                if sent.is_err() && meta.thread_id.is_some() {
                    let _ = bot3.send_message(chat_id, MSG).await;
                }
            });
        });
        // Safety: pool has no other Arc clones yet at this point.
        *pool.evict_notifier.lock().unwrap() = Some(notifier);
    }

    // Idle session cleanup loop — 1 min interval for testing (switch to 5 min in prod).
    // TTL: 2 min for testing (switch to 30 min in prod).
    const CLEANUP_INTERVAL_SECS: u64 = 900;
    const SESSION_TTL_SECS: u64 = 7200;
    {
        let pool2 = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                std::time::Duration::from_secs(CLEANUP_INTERVAL_SECS)
            );
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                pool2.cleanup_idle(SESSION_TTL_SECS).await;
            }
        });
    }

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let pool = pool.clone();
        let allowed_users = allowed_users.clone();
        let bot_username = bot_username.clone();
        let mode = mode.clone();
        async move {
            let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
            tracing::info!(chat_id = %msg.chat.id, user_id, thread_id = ?msg.thread_id, text = ?msg.text(), "raw message received");

            // Auth check
            if !allowed_users.is_empty() && !allowed_users.contains(&user_id) {
                return Ok(());
            }

            let prompt = match msg.text() {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => return Ok(()),
            };
            let user_msg_id = msg.id;

            let is_forum = matches!(
                &msg.chat.kind,
                ChatKind::Public(p) if matches!(&p.kind, PublicChatKind::Supergroup(s) if s.is_forum)
            );
            let in_real_topic = is_forum && msg.thread_id.map_or(false, |t| t.0 != MessageId(1));
            let is_group = !msg.chat.is_private();

            // ── Bot commands ─────────────────────────────────────────────────
            if prompt.starts_with('!') && !prompt.starts_with("!kiro") {
                let ctx = match get_or_create_thread(&bot, &msg, false).await {
                    Ok(c) => c,
                    Err(_) => return Ok(()),
                };
                let reply = match prompt.trim() {
                    "!status" => pool.session_status(&ctx.session_key).await,
                    "!stop" => {
                        pool.remove_session(&ctx.session_key).await;
                        "✅ Session stopped.".to_string()
                    }
                    "!restart" => {
                        pool.remove_session(&ctx.session_key).await;
                        "♻️ Session cleared. Send a message to start fresh.".to_string()
                    }
                    _ => return Ok(()),
                };
                let chat_id = msg.chat.id;
                let mut req = bot.send_message(chat_id, reply);
                if let Some(tid) = ctx.thread_id {
                    req = req.message_thread_id(tid);
                } else {
                    req = req.reply_parameters(ReplyParameters::new(msg.id));
                }
                let _ = req.await;
                return Ok(());
            }
            // ─────────────────────────────────────────────────────────────────

            // Strip `!kiro` prefix if present.
            let is_kiro_cmd = prompt.starts_with("!kiro");
            let prompt = if is_kiro_cmd {
                prompt.trim_start_matches("!kiro").trim().to_string()
            } else {
                prompt
            };
            if prompt.is_empty() { return Ok(()); }

            // ── Group chat gate ───────────────────────────────────────────────
            // In a group (not inside a real topic):
            //   - `!kiro` → only allowed by topic_creator_id; creates a new topic.
            //   - plain message → must @mention the bot; responds in-place (no topic).
            // ── Mode-based routing ────────────────────────────────────────────
            // Personal: any message in #general/All → new topic; inside topic → always reply.
            // Team:     only !kiro creates topics; inside topic → silent unless @mentioned.
            let silent_mode;
            match mode {
                ChatMode::Personal => {
                    // Outside a topic in a group: any message triggers topic creation.
                    // (is_kiro_cmd is irrelevant here — fall through for all messages)
                    silent_mode = false;
                }
                ChatMode::Team => {
                    let is_mentioned = bot_username.as_deref().map_or(true, |name| {
                        prompt.to_lowercase().contains(&format!("@{}", name))
                    });
                    if is_group && !in_real_topic {
                        if is_kiro_cmd {
                            if let Some(creator) = topic_creator_id {
                                if user_id != creator { return Ok(()); }
                            }
                            silent_mode = false;
                        } else {
                            // plain message or @mention in #general → buffer silently (no topic)
                            silent_mode = !is_mentioned;
                        }
                    } else {
                        silent_mode = in_real_topic && !is_mentioned;
                    }
                }
            }
            // ─────────────────────────────────────────────────────────────────

            let chat_id = msg.chat.id;

            // In personal mode every message can create a topic; in team mode only !kiro.
            let may_create_topic = is_kiro_cmd || mode == ChatMode::Personal;

            // Resolve thread context
            let ctx = match get_or_create_thread(&bot, &msg, may_create_topic).await {
                Ok(c) => c,
                Err(e) => {
                    if is_kiro_cmd {
                        error!("topic creation failed: {e}");
                        let _ = bot.send_message(chat_id, format!("⚠️ Failed to create topic: {e}"))
                            .reply_parameters(ReplyParameters::new(user_msg_id))
                            .await;
                    } else {
                        tracing::debug!("thread setup skipped: {e}");
                    }
                    return Ok(());
                }
            };

            if let Err(e) = pool.get_or_create(&ctx.session_key).await {
                if !silent_mode {
                    let _ = bot.send_message(chat_id, format!("⚠️ {e}"))
                        .reply_parameters(ReplyParameters::new(user_msg_id))
                        .await;
                }
                error!("pool error: {e}");
                return Ok(());
            }

            pool.register_meta(&ctx.session_key, SessionMeta {
                chat_id: chat_id.0,
                thread_id: ctx.thread_id.map(|t| t.0 .0),
            }).await;

            // Prefix with sender name in shared topics.
            let name = msg.from.as_ref().map(|u| u.first_name.as_str()).unwrap_or("User");
            let attributed_prompt = if in_real_topic {
                format!("[{}]: {}", name, prompt)
            } else {
                prompt.clone()
            };

            if silent_mode {
                // React 👀, run Kiro for context awareness, discard the reply.
                set_reaction(&bot, chat_id, user_msg_id, EMOJI_QUEUED).await;
                let _ = silent_prompt(&pool, &ctx.session_key, &attributed_prompt).await;
                clear_reaction(&bot, chat_id, user_msg_id).await;
                return Ok(());
            }

            // Send initial "..." placeholder.
            let thinking = {
                let mut req = bot.send_message(chat_id, "...");
                if let Some(tid) = ctx.thread_id {
                    req = req.message_thread_id(tid);
                } else {
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

            set_reaction(&bot, chat_id, user_msg_id, EMOJI_THINKING).await;

            let result = stream_prompt(
                &pool, &ctx.session_key, &attributed_prompt,
                &bot, chat_id, thinking.id, user_msg_id, ctx.thread_id,
            ).await;

            match &result {
                Ok(()) => {
                    set_reaction(&bot, chat_id, user_msg_id, EMOJI_DONE).await;
                    // Rename new topics with a Kiro-generated title
                    if ctx.is_new_topic {
                        if let Some(tid) = ctx.thread_id {
                            rename_topic(&pool, &ctx.session_key, &prompt, &bot, chat_id, tid).await;
                        }
                    }
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

// ── Topic rename ─────────────────────────────────────────────────────────────

async fn rename_topic(
    pool: &SessionPool,
    session_key: &str,
    original_prompt: &str,
    bot: &Bot,
    chat_id: ChatId,
    thread_id: ThreadId,
) {
    let title_prompt = format!(
        "Reply with ONLY a short topic title (max 40 chars, no quotes) for this message: {}",
        original_prompt
    );
    let rx = pool.with_connection(session_key, |conn| {
        let p = title_prompt.clone();
        Box::pin(async move {
            let (rx, _) = conn.session_prompt(&p).await?;
            Ok(rx)
        })
    }).await;

    if let Ok(mut rx) = rx {
        let mut text = String::new();
        while let Some(msg) = rx.recv().await {
            if msg.id.is_some() { break; }
            if let Some(crate::acp::AcpEvent::Text(t)) = crate::acp::classify_notification(&msg) {
                text.push_str(&t);
            }
        }
        let _ = pool.with_connection(session_key, |conn| Box::pin(async move {
            conn.prompt_done().await;
            Ok(())
        })).await;

        let name = format!("🤖 {}", text.trim().chars().take(40).collect::<String>());
        let _ = bot.edit_forum_topic(chat_id, thread_id).name(name).await;
    }
}

// ── Silent prompt — run Kiro, buffer reply, no message sent ──────────────────

async fn silent_prompt(pool: &SessionPool, session_key: &str, prompt: &str) -> anyhow::Result<String> {
    // Start the prompt (briefly holds write lock), then drain outside the lock.
    let prompt = prompt.to_string();
    let rx = pool.with_connection(session_key, |conn| {
        let prompt = prompt.clone();
        Box::pin(async move {
            let (rx, _) = conn.session_prompt(&prompt).await?;
            Ok(rx)
        })
    }).await?;

    let mut rx = rx;
    let mut text = String::new();
    while let Some(msg) = rx.recv().await {
        if msg.id.is_some() { break; }
        if let Some(AcpEvent::Text(t)) = classify_notification(&msg) {
            text.push_str(&t);
        }
    }

    pool.with_connection(session_key, |conn| Box::pin(async move {
        conn.prompt_done().await;
        Ok(())
    })).await?;

    Ok(text)
}

// ── Streaming prompt with live edits ─────────────────────────────────────────

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

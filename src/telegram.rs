use crate::acp::{classify_notification, AcpEvent, SessionPool, SessionMeta};
use crate::config::ChatMode;
use crate::format;
use std::collections::HashSet;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::net::Download;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::types::{
    ChatKind, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, PublicChatKind, ReactionType,
    ReplyParameters, ThreadId,
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

const SESSION_PREAMBLE: &str = "\
[System: You are running inside agent-broker, a Telegram bot bridge. \
You are NOT in a terminal — your responses appear in a Telegram topic thread. \
Keep replies concise: state key outcomes and important steps only, skip verbose narration \
(tool calls are already shown to the user separately). \
When ending a response with a conclusion or summary, separate it with a line containing \
only \"---\" so it can be sent as a distinct message. \
Before executing any multi-step or destructive action, propose a plan and wait for confirmation.\
\n\n\
Issue Triage Guide (openabdev/openab):\n\
1. Confirm type: bug / feature / guidance\n\
2. Verify claims against source code or official docs before accepting as valid\n\
3. Set exactly one priority: p0 🔴 critical | p1 🟠 high | p2 🟡 medium | p3 🟤 low\n\
4. Remove needs-triage label when done\n\
Response must include an ASCII flow diagram showing where things break.\n\
Draft response for human approval before posting to the issue.]";

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
        let topic_name: String = if prompt_preview.is_empty() {
            format!("📷 {}", user_name)
        } else {
            format!("{}: {}", user_name, prompt_preview)
        }.chars().take(128).collect();

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

pub async fn run(pool: Arc<SessionPool>, bot_token: String, allowed_users: HashSet<i64>, topic_creator_id: Option<i64>, model_admin_ids: Vec<i64>, mode: ChatMode) {
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

    let pool_cb = pool.clone();
    let bot2 = bot.clone();

    let handler = dptree::entry()
        .branch(
            Update::filter_message().endpoint(move |bot: Bot, msg: Message| {
                let pool = pool.clone();
                let allowed_users = allowed_users.clone();
                let bot_username = bot_username.clone();
                let mode = mode.clone();
                let model_admin_ids = model_admin_ids.clone();
                async move {
                    handle_message(bot, msg, pool, allowed_users, bot_username, mode, topic_creator_id, model_admin_ids).await
                }
            })
        )
        .branch(
            Update::filter_callback_query().endpoint(move |bot: Bot, q: CallbackQuery| {
                let pool = pool_cb.clone();
                async move { handle_callback(bot, q, pool).await }
            })
        );

    Dispatcher::builder(bot2, handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

async fn handle_message(
    bot: Bot,
    msg: Message,
    pool: Arc<SessionPool>,
    allowed_users: HashSet<i64>,
    bot_username: Option<String>,
    mode: ChatMode,
    topic_creator_id: Option<i64>,
    model_admin_ids: Vec<i64>,
) -> ResponseResult<()> {
            let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
            tracing::info!(chat_id = %msg.chat.id, user_id, thread_id = ?msg.thread_id, text = ?msg.text(), "raw message received");

            // Auth check
            if !allowed_users.is_empty() && !allowed_users.contains(&user_id) {
                return Ok(());
            }

            // Accept text, photos, and documents.
            // raw_image: raw bytes for images — model gate applied inside stream_prompt.
            // extra_blocks: pre-resolved ContentBlocks for documents (markitdown runs here).
            let (prompt, raw_image, extra_blocks) = if let Some(photos) = msg.photo() {
                let caption = msg.caption().unwrap_or("").to_string();
                let photo = match photos.iter().max_by_key(|p| p.width * p.height) {
                    Some(p) => p,
                    None => return Ok(()),
                };
                let file = match bot.get_file(&photo.file.id).await {
                    Ok(f) => f,
                    Err(e) => {
                        error!("get_file failed: {e}");
                        let _ = bot.send_message(msg.chat.id, "⚠️ Failed to retrieve image.")
                            .reply_parameters(ReplyParameters::new(msg.id))
                            .await;
                        return Ok(());
                    }
                };
                let mut bytes: Vec<u8> = Vec::new();
                if let Err(e) = bot.download_file(&file.path, &mut bytes).await {
                    error!("download_file failed: {e}");
                    let _ = bot.send_message(msg.chat.id, "⚠️ Failed to download image.")
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await;
                    return Ok(());
                }
                (caption, Some((bytes, "image/jpeg".to_string())), vec![])
            } else if let Some(doc) = msg.document() {
                let caption = msg.caption().unwrap_or("").to_string();
                let filename = doc.file_name.clone().unwrap_or_else(|| "document".to_string());
                let file = match bot.get_file(&doc.file.id).await {
                    Ok(f) => f,
                    Err(e) => {
                        error!("get_file failed: {e}");
                        let _ = bot.send_message(msg.chat.id, "⚠️ Failed to retrieve file.")
                            .reply_parameters(ReplyParameters::new(msg.id))
                            .await;
                        return Ok(());
                    }
                };
                let mut bytes: Vec<u8> = Vec::new();
                if let Err(e) = bot.download_file(&file.path, &mut bytes).await {
                    error!("download_file failed: {e}");
                    let _ = bot.send_message(msg.chat.id, "⚠️ Failed to download file.")
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await;
                    return Ok(());
                }
                let blocks = crate::media::resolve(
                    crate::media::MediaInput::Document { bytes, filename },
                    None,
                ).await;
                (caption, None, blocks)
            } else {
                match msg.text() {
                    Some(t) if !t.is_empty() => (t.to_string(), None, vec![]),
                    _ => return Ok(()),
                }
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

                // !model [value] — dedicated model picker shortcut
                // !cmd /model or !cmd /agent — generic slash command bridge
                if prompt.trim() == "!model" || prompt.starts_with("!model ")
                    || prompt.trim() == "!cmd" || prompt.starts_with("!cmd ")
                {
                    let chat_id = msg.chat.id;

                    // In team mode: only model_admin_ids (falling back to topic_creator_id) may run this.
                    // In personal mode: no extra gate needed (allowed_users already covers it).
                    if mode == ChatMode::Team {
                        let can = can_change_model(user_id, &model_admin_ids, topic_creator_id);
                        if !can {
                            let mut req = bot.send_message(chat_id, "⛔ Only admins can change the model.");
                            if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                            else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                            let _ = req.await;
                            return Ok(());
                        }
                    }

                    // !model only makes sense inside an active topic session.
                    if !in_real_topic {
                        let mut req = bot.send_message(chat_id, "⚠️ Use `!model` inside a topic where a session is active.");
                        if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                        else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                        let _ = req.await;
                        return Ok(());
                    }

                    let (slash_cmd, value) = parse_cmd_prompt(&prompt);

                    // Ensure session exists so we have options
                    let _ = pool.get_or_create(&ctx.session_key).await;

                    if slash_cmd.is_empty() || !slash_cmd.starts_with('/') {
                        let mut req = bot.send_message(chat_id, "Usage: `!model` or `!cmd /agent`");
                        if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                        else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                        let _ = req.await;
                        return Ok(());
                    }

                    let options = pool.get_slash_options(&ctx.session_key, &slash_cmd).await;

                    if value.is_empty() {
                        // Show inline keyboard
                        if options.is_empty() {
                            let mut req = bot.send_message(chat_id, format!("⚠️ No options available for `{slash_cmd}`. Try sending a message first to start a session."));
                            if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                            else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                            let _ = req.await;
                        } else {
                            let buttons: Vec<Vec<InlineKeyboardButton>> = options.iter().map(|o| {
                                let label = if o.current { format!("✓ {}", o.name) } else { o.name.clone() };
                                vec![InlineKeyboardButton::callback(
                                    label,
                                    format!("cmd:{}:{}:{}", ctx.session_key, slash_cmd, o.id),
                                )]
                            }).collect();
                            let mut req = bot.send_message(chat_id, format!("Select for `{slash_cmd}`:"))
                                .reply_markup(InlineKeyboardMarkup::new(buttons));
                            if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                            else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                            let _ = req.await;
                        }
                    } else {
                        // Direct: !model claude-sonnet-4 / !cmd /model claude-sonnet-4 → silent prompt
                        let prompt_text = format!("{slash_cmd} {value}");
                        let reply_text = match silent_prompt(&pool, &ctx.session_key, &prompt_text).await {
                            Ok(_) => format!("✅ Sent: `{prompt_text}`"),
                            Err(e) => {
                                tracing::error!(session_key = %ctx.session_key, cmd = %prompt_text, error = %e, "silent_prompt failed");
                                format!("⚠️ Failed to apply `{prompt_text}`: {e}")
                            }
                        };
                        let mut req = bot.send_message(chat_id, reply_text);
                        if let Some(tid) = ctx.thread_id { req = req.message_thread_id(tid); }
                        else { req = req.reply_parameters(ReplyParameters::new(msg.id)); }
                        let _ = req.await;
                    }
                    return Ok(());
                }

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
            if prompt.is_empty() && raw_image.is_none() && extra_blocks.is_empty() { return Ok(()); }

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
                            // Exception: photos always get a topic so the model can describe them.
                            silent_mode = raw_image.is_none() && extra_blocks.is_empty() && !is_mentioned;
                        }
                    } else {
                        silent_mode = in_real_topic && !is_mentioned;
                    }
                }
            }
            // ─────────────────────────────────────────────────────────────────

            let chat_id = msg.chat.id;

            // In personal mode every message can create a topic; in team mode only !kiro or photos.
            let may_create_topic = is_kiro_cmd || mode == ChatMode::Personal || raw_image.is_some() || !extra_blocks.is_empty();

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

            if ctx.is_new_topic {
                pool.set_pending_context(&ctx.session_key, SESSION_PREAMBLE.to_string()).await;
            }

            pool.register_meta(&ctx.session_key, SessionMeta {
                chat_id: chat_id.0,
                thread_id: ctx.thread_id.map(|t| t.0 .0),
            }).await;

            // Prefix with sender name in shared topics.
            // For image-only messages (no caption), use "[image]" as placeholder prompt
            // so kiro knows media was sent even if caption is empty.
            let name = msg.from.as_ref().map(|u| u.first_name.as_str()).unwrap_or("User");
            let effective_prompt = if prompt.is_empty() && raw_image.is_some() {
                "[image]".to_string()
            } else if prompt.is_empty() && !extra_blocks.is_empty() {
                "[file]".to_string()
            } else {
                prompt.clone()
            };
            let attributed_prompt = if in_real_topic {
                format!("[{}]: {}", name, effective_prompt)
            } else {
                effective_prompt.clone()
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

            pool.record_user_message(&ctx.session_key, &attributed_prompt).await;
            let result = stream_prompt(
                &pool, &ctx.session_key, &attributed_prompt, raw_image, extra_blocks,
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

// ── Callback query handler (inline keyboard) ─────────────────────────────────

async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    pool: Arc<SessionPool>,
) -> ResponseResult<()> {
    let data = match q.data.as_deref() {
        Some(d) if d.starts_with("cmd:") => d,
        _ => {
            let _ = bot.answer_callback_query(&q.id).await;
            return Ok(());
        }
    };

    // Format: "cmd:<session_key>:<slash_cmd>:<value>"
    // session_key may contain ':', so split into exactly 4 parts from the right
    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 {
        let _ = bot.answer_callback_query(&q.id).await;
        return Ok(());
    }
    let session_key = parts[1];
    let slash_cmd = parts[2];
    let value = parts[3];

    let prompt_text = format!("{slash_cmd} {value}");
    let _ = silent_prompt(&pool, session_key, &prompt_text).await;

    // Update keyboard to reflect new selection
    if let Some(msg) = &q.message {
        let options = pool.get_slash_options(session_key, slash_cmd).await;
        if !options.is_empty() {
            let buttons: Vec<Vec<InlineKeyboardButton>> = options.iter().map(|o| {
                let label = if o.id == value { format!("✓ {}", o.name) } else { o.name.clone() };
                vec![InlineKeyboardButton::callback(
                    label,
                    format!("cmd:{}:{}:{}", session_key, slash_cmd, o.id),
                )]
            }).collect();
            let _ = bot.edit_message_reply_markup(msg.chat().id, msg.id())
                .reply_markup(InlineKeyboardMarkup::new(buttons))
                .await;
        }
    }

    let _ = bot.answer_callback_query(&q.id)
        .text(format!("✅ {slash_cmd} → {value}"))
        .await;

    Ok(())
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
            let (rx, _) = conn.session_prompt(&p, vec![]).await?;
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
            let (rx, _) = conn.session_prompt(&prompt, vec![]).await?;
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
    image: Option<(Vec<u8>, String)>,
    doc_blocks: Vec<crate::acp::ContentBlock>,
    bot: &Bot,
    chat_id: ChatId,
    msg_id: MessageId,
    user_msg_id: MessageId,
    thread_id: Option<ThreadId>,
) -> anyhow::Result<()> {
    let prompt = prompt.to_string();
    let bot = bot.clone();
    let session_key = session_key.to_string();

    let (_final_content, timed_out, agent_died, partial_summary) = pool.with_connection(&session_key, |conn| {
        let prompt = prompt.clone();
        let bot = bot.clone();
        let session_key = session_key.clone();
        let image = image;
        let doc_blocks = doc_blocks;
        Box::pin(async move {
            let reset = conn.session_reset;
            conn.session_reset = false;

            // Resolve image → ContentBlock with model gate, then append any doc blocks
            let mut extra_blocks = if let Some((bytes, mime)) = image {
                let blocks = crate::media::resolve(
                    crate::media::MediaInput::Image { bytes, mime },
                    conn.current_model_id.as_deref(),
                ).await;
                if blocks.is_empty() {
                    let model = conn.current_model_id.as_deref().unwrap_or("current model");
                    let _ = bot.edit_message_text(
                        chat_id, msg_id,
                        format!("⚠️ `{model}` doesn't support images. Use `!model` to switch to Claude. Responding to caption only.")
                    ).await;
                }
                blocks
            } else {
                vec![]
            };
            extra_blocks.extend(doc_blocks);

            let (mut rx, _) = conn.session_prompt(&prompt, extra_blocks).await?;

            let mut text_buf = String::new();
            let mut tool_lines: Vec<String> = Vec::new();
            let mut last_sent = String::new();
            let mut current_msg_id = msg_id;
            let mut last_edit = tokio::time::Instant::now();
            let prompt_start = tokio::time::Instant::now();
            let mut timed_out = false;
            let mut agent_died = false;

            // WORKAROUND: These constants are hardcoded. Ideally the ACP protocol
            // would provide structured lifecycle signals (heartbeat, progress, cancellation)
            // so the broker doesn't need to guess via polling. Revisit when ACP matures.
            const HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
            const ALIVE_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            'outer: loop {
                tokio::select! {
                    msg = rx.recv() => {
                        let notification = match msg {
                            Some(n) => n,
                            None => break,
                        };

                        if notification.id.is_some() {
                            // WORKAROUND: 200ms drain window to catch late-arriving text chunks
                            // that race against end_turn. Proper fix: sequence numbers in ACP.
                            let drain_until = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
                            while let Ok(Some(n)) = tokio::time::timeout_at(drain_until, rx.recv()).await {
                                if let Some(AcpEvent::Text(t)) = classify_notification(&n) {
                                    text_buf.push_str(&t);
                                }
                            }
                            break;
                        }

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
                    }
                    // WORKAROUND: Poll liveness every 30s since ACP has no heartbeat.
                    // stream_prompt still holds the write lock — a hung task blocks other
                    // messages for up to HARD_TIMEOUT. Proper fix: release lock after
                    // session_prompt, drain outside (same pattern as silent_prompt).
                    _ = tokio::time::sleep(ALIVE_CHECK) => {
                        if !conn.alive() {
                            tracing::warn!(session_key, "agent process died mid-prompt");
                            agent_died = true;
                            break 'outer;
                        }
                        if prompt_start.elapsed() > HARD_TIMEOUT {
                            tracing::warn!(session_key, "prompt exceeded 30min hard timeout");
                            timed_out = true;
                            break 'outer;
                        }
                        continue;
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

            // Build partial summary from whatever we captured — used for session recovery below.
            let partial_summary = if timed_out || agent_died {
                compose_display(&tool_lines, &text_buf)
            } else {
                String::new()
            };

            // Append user-facing notice for abnormal exits
            if timed_out {
                text_buf.push_str("\n\n⚠️ _Timed out after 30 minutes. Use !restart to start a fresh session._");
            } else if agent_died {
                text_buf.push_str("\n\n⚠️ _Agent process stopped unexpectedly. Use !restart to recover._");
            }

            let final_content = compose_display(&tool_lines, &text_buf);
            let final_content = if final_content.trim().is_empty() && !tool_lines.is_empty() {
                format!("{}\n\n_Task completed._", tool_lines.join("\n"))
            } else if final_content.trim().is_empty() {
                "_(no response)_".to_string()
            } else {
                final_content
            };
            // If Kiro appended a conclusion after "---", send it as a separate message.
            if let Some(sep) = final_content.rfind("\n---\n") {
                let body = final_content[..sep].trim();
                let conclusion = final_content[sep + 5..].trim();
                if !body.is_empty() && !conclusion.is_empty() {
                    send_chunks(&bot, chat_id, current_msg_id, body, thread_id).await;
                    let mut req = bot.send_message(chat_id, conclusion);
                    if let Some(tid) = thread_id { req = req.message_thread_id(tid); }
                    let _ = req.await;
                } else {
                    send_chunks(&bot, chat_id, current_msg_id, &final_content, thread_id).await;
                }
            } else {
                send_chunks(&bot, chat_id, current_msg_id, &final_content, thread_id).await;
            }

            Ok((final_content, timed_out, agent_died, partial_summary))
        })
    })
    .await?;

    // After lock is released: store partial summary and evict dead/timed-out session
    // so the next message gets a fresh session with injected context.
    // Works for both personal and team mode — recovery is session-key based, mode-agnostic.
    if timed_out || agent_died {
        pool.store_partial_summary(&session_key, partial_summary).await;
        pool.remove_session(&session_key).await;
        tracing::info!(session_key, timed_out, agent_died, "session evicted after abnormal exit, partial summary stored");
    }

    Ok(())
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

/// Returns true if `user_id` is allowed to run `!model` / `!cmd`.
/// Checks `model_admin_ids` first; falls back to `topic_creator_id`; if neither is set, allows all.
fn can_change_model(user_id: i64, model_admin_ids: &[i64], topic_creator_id: Option<i64>) -> bool {
    if !model_admin_ids.is_empty() {
        return model_admin_ids.contains(&user_id);
    }
    if let Some(creator) = topic_creator_id {
        return user_id == creator;
    }
    true // no restriction configured
}

/// Parse `!model [value]` or `!cmd /slash [value]` into `(slash_cmd, value)`.
/// Returns `("", "")` for bare `!cmd` with no slash argument.
fn parse_cmd_prompt(prompt: &str) -> (String, String) {
    let arg = if prompt.starts_with("!model") {
        format!("/model {}", prompt.trim_start_matches("!model").trim())
    } else {
        prompt.trim_start_matches("!cmd").trim().to_string()
    };
    let arg = arg.trim();
    let mut parts = arg.splitn(2, ' ');
    let slash_cmd = parts.next().unwrap_or("").to_string();
    let value = parts.next().unwrap_or("").trim().to_string();
    (slash_cmd, value)
}

#[cfg(test)]
mod tests {
    use super::{can_change_model, parse_cmd_prompt};

    // ── !model variants ───────────────────────────────────────────────────────

    #[test]
    fn model_bare() {
        let (cmd, val) = parse_cmd_prompt("!model");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "");
    }

    #[test]
    fn model_with_value() {
        let (cmd, val) = parse_cmd_prompt("!model claude-sonnet-4");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "claude-sonnet-4");
    }

    #[test]
    fn model_extra_spaces() {
        let (cmd, val) = parse_cmd_prompt("!model   claude-sonnet-4  ");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "claude-sonnet-4");
    }

    #[test]
    fn model_only_spaces() {
        // "!model   " — no real value, should show picker
        let (cmd, val) = parse_cmd_prompt("!model   ");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "");
    }

    // ── !cmd variants ─────────────────────────────────────────────────────────

    #[test]
    fn cmd_bare() {
        // "!cmd" alone — no slash arg, should hit usage error
        let (cmd, val) = parse_cmd_prompt("!cmd");
        assert_eq!(cmd, "");
        assert_eq!(val, "");
    }

    #[test]
    fn cmd_slash_model_bare() {
        let (cmd, val) = parse_cmd_prompt("!cmd /model");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "");
    }

    #[test]
    fn cmd_slash_model_with_value() {
        let (cmd, val) = parse_cmd_prompt("!cmd /model claude-sonnet-4");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "claude-sonnet-4");
    }

    #[test]
    fn cmd_slash_agent() {
        let (cmd, val) = parse_cmd_prompt("!cmd /agent");
        assert_eq!(cmd, "/agent");
        assert_eq!(val, "");
    }

    #[test]
    fn cmd_slash_agent_with_value() {
        let (cmd, val) = parse_cmd_prompt("!cmd /agent kiro-cli");
        assert_eq!(cmd, "/agent");
        assert_eq!(val, "kiro-cli");
    }

    // ── Corner cases ──────────────────────────────────────────────────────────

    #[test]
    fn cmd_no_slash_prefix() {
        // "!cmd model" — missing leading slash, should fail the starts_with('/') guard
        let (cmd, val) = parse_cmd_prompt("!cmd model");
        assert!(!cmd.starts_with('/'), "should not have slash prefix");
        assert_eq!(val, "");
    }

    #[test]
    fn cmd_bare_slash() {
        // "!cmd /" — slash with no name
        let (cmd, val) = parse_cmd_prompt("!cmd /");
        assert_eq!(cmd, "/");
        assert_eq!(val, "");
    }

    #[test]
    fn cmd_value_with_spaces() {
        // value containing spaces — only first space splits cmd from value
        let (cmd, val) = parse_cmd_prompt("!cmd /model claude sonnet 4");
        assert_eq!(cmd, "/model");
        assert_eq!(val, "claude sonnet 4");
    }

    // ── can_change_model ──────────────────────────────────────────────────────

    #[test]
    fn model_admin_ids_set_allows_listed_user() {
        assert!(can_change_model(111, &[111, 222], Some(333)));
    }

    #[test]
    fn model_admin_ids_set_blocks_unlisted_user() {
        assert!(!can_change_model(999, &[111, 222], Some(333)));
    }

    #[test]
    fn model_admin_ids_empty_falls_back_to_creator() {
        assert!(can_change_model(333, &[], Some(333)));
        assert!(!can_change_model(999, &[], Some(333)));
    }

    #[test]
    fn no_restriction_allows_all() {
        assert!(can_change_model(999, &[], None));
    }
}
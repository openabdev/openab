use crate::acp::{classify_notification, AcpEvent, ContentBlock, SessionPool};
use crate::config::ReactionsConfig;
use crate::error_display::{format_coded_error, format_user_error};
use crate::format;
use crate::reactions::StatusReactionController;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use std::sync::LazyLock;
use serenity::async_trait;
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, MessageId};
use serenity::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{debug, error, info};

/// Reusable HTTP client for downloading Discord attachments.
/// Built once with a 30s timeout and rustls TLS (no native-tls deps).
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static HTTP client must build")
});

pub struct Handler {
    pub pool: Arc<SessionPool>,
    pub allowed_channels: HashSet<u64>,
    pub allowed_users: HashSet<u64>,
    pub reactions_config: ReactionsConfig,
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
        if !in_thread && !is_mentioned {
            return;
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

        // No text and no image attachments → skip to avoid wasting session slots
        if prompt.is_empty() && msg.attachments.is_empty() {
            return;
        }

        // Build content blocks: text + image attachments
        let mut content_blocks = vec![];

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

        // Add text block (always, even if empty, we still send for sender context)
        content_blocks.push(ContentBlock::Text {
            text: prompt_with_sender.clone(),
        });

        // Add image attachments
        if !msg.attachments.is_empty() {
            for attachment in &msg.attachments {
                if let Some(content_block) = download_and_encode_image(attachment).await {
                    debug!(url = %attachment.url, filename = %attachment.filename, "adding image attachment");
                    content_blocks.push(content_block);
                } else {
                    error!(
                        url = %attachment.url,
                        filename = %attachment.filename,
                        "failed to download image attachment"
                    );
                }
            }
        }

        tracing::debug!(
            text_len = prompt_with_sender.len(),
            num_attachments = msg.attachments.len(),
            in_thread,
            "processing"
        );

        // Note: image-only messages (no text) are intentionally allowed since
        // prompt_with_sender always includes the non-empty sender_context XML.
        // The guard above (prompt.is_empty() && no attachments) handles stickers/embeds.

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
            let msg = format_user_error(&e.to_string());
            let _ = edit(&ctx, thread_channel, thinking_msg.id, &format!("⚠️ {}", msg)).await;
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

        // Stream prompt with live edits (pass content blocks instead of just text)
        let result = stream_prompt(
            &self.pool,
            &thread_key,
            content_blocks,
            &ctx,
            thread_channel,
            thinking_msg.id,
            reactions.clone(),
        )
        .await;

        match &result {
            Ok(()) => reactions.set_done().await,
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

/// Download a Discord image attachment and encode it as an ACP image content block.
///
/// Discord attachment URLs are temporary and expire, so we must download
/// and encode the image data immediately. The ACP ImageContent schema
/// requires `{ data: base64_string, mimeType: "image/..." }`.
///
/// Security: rejects non-image attachments (by content-type or extension)
/// and files larger than 10MB to prevent OOM/abuse.
async fn download_and_encode_image(attachment: &serenity::model::channel::Attachment) -> Option<ContentBlock> {
    const MAX_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

    let url = &attachment.url;
    if url.is_empty() {
        return None;
    }

    // Determine media type — prefer content-type header, fallback to extension
    let media_type = attachment
        .content_type
        .as_deref()
        .or_else(|| {
            attachment
                .filename
                .rsplit('.')
                .next()
                .and_then(|ext| match ext.to_lowercase().as_str() {
                    "png" => Some("image/png"),
                    "jpg" | "jpeg" => Some("image/jpeg"),
                    "gif" => Some("image/gif"),
                    "webp" => Some("image/webp"),
                    _ => None,
                })
        });

    // Validate that it's actually an image
    let Some(mime) = media_type else {
        debug!(filename = %attachment.filename, "skipping non-image attachment (no matching content-type or extension)");
        return None;
    };
    // Strip MIME type parameters (e.g. "image/jpeg; charset=utf-8" → "image/jpeg")
    // Downstream LLM APIs (Claude, OpenAI, Gemini) reject MIME types with parameters
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    if !mime.starts_with("image/") {
        debug!(filename = %attachment.filename, mime = %mime, "skipping non-image attachment");
        return None;
    }

    // Size check before downloading
    if u64::from(attachment.size) > MAX_SIZE {
        error!(
            filename = %attachment.filename,
            size = attachment.size,
            max = MAX_SIZE,
            "image attachment exceeds 10MB limit"
        );
        return None;
    }

    // Download using the static reusable client
    let response = match HTTP_CLIENT.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            error!("failed to download image {}: {}", url, e);
            return None;
        }
    };

    if !response.status().is_success() {
        error!("HTTP error downloading image {}: {}", url, response.status());
        return None;
    }

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!("failed to read image bytes from {}: {}", url, e);
            return None;
        }
    };

    // Final size check after download (defense in depth)
    if bytes.len() as u64 > MAX_SIZE {
        error!(
            filename = %attachment.filename,
            size = bytes.len(),
            "downloaded image exceeds 10MB limit after decode"
        );
        return None;
    }

    let encoded = BASE64.encode(bytes.as_ref());
    Some(ContentBlock::Image {
        media_type: mime.to_string(),
        data: encoded,
    })
}

async fn edit(ctx: &Context, ch: ChannelId, msg_id: MessageId, content: &str) -> serenity::Result<Message> {
    ch.edit_message(&ctx.http, msg_id, serenity::builder::EditMessage::new().content(content)).await
}

async fn stream_prompt(
    pool: &SessionPool,
    thread_key: &str,
    content_blocks: Vec<ContentBlock>,
    ctx: &Context,
    channel: ChannelId,
    msg_id: MessageId,
    reactions: Arc<StatusReactionController>,
) -> anyhow::Result<()> {
    let reactions = reactions.clone();

    pool.with_connection(thread_key, |conn| {
        let content_blocks = content_blocks.clone();
        let ctx = ctx.clone();
        let reactions = reactions.clone();
        Box::pin(async move {
            let reset = conn.session_reset;
            conn.session_reset = false;

            let (mut rx, _): (_, _) = conn.session_prompt(content_blocks).await?;
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
            let mut response_error: Option<String> = None;
            while let Some(notification) = rx.recv().await {
                if notification.id.is_some() {
                    // Capture error from ACP response to display in Discord
                    if let Some(ref err) = notification.error {
                        response_error = Some(format_coded_error(err.code, &err.message));
                    }
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
            // If ACP returned both an error and partial text, show both.
            // This can happen when the agent started producing content before hitting an error
            // (e.g. context length limit, rate limit mid-stream). Showing both gives users
            // full context rather than hiding the partial response.
            let final_content = if final_content.is_empty() {
                if let Some(err) = response_error {
                    format!("⚠️ {}", err)
                } else {
                    "_(no response)_".to_string()
                }
            } else if let Some(err) = response_error {
                format!("⚠️ {}\n\n{}", err, final_content)
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

static MENTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<@[!&]?\d+>").unwrap()
});

fn strip_mention(content: &str) -> String {
    MENTION_RE.replace_all(content, "").trim().to_string()
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


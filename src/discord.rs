use crate::acp::{classify_notification, AcpEvent, ContentBlock, SessionPool};
use crate::config::ReactionsConfig;
use crate::error_display::{format_coded_error, format_user_error};
use crate::format;
use crate::reactions::StatusReactionController;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::ImageReader;
use std::io::Cursor;
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
                        .is_some_and(|pid| self.allowed_channels.contains(&pid.get()));
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

/// Maximum dimension (width or height) for resized images.
/// Matches OpenClaw's DEFAULT_IMAGE_MAX_DIMENSION_PX.
const IMAGE_MAX_DIMENSION_PX: u32 = 1200;

/// JPEG quality for compressed output (OpenClaw uses progressive 85→35;
/// we start at 75 which is a good balance of quality vs size).
const IMAGE_JPEG_QUALITY: u8 = 75;

/// Download a Discord image attachment, resize/compress it, then base64-encode
/// as an ACP image content block.
///
/// Large images are resized so the longest side is at most 1200px and
/// re-encoded as JPEG at quality 75. This keeps the base64 payload well
/// under typical JSON-RPC transport limits (~200-400KB after encoding).
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

    let Some(mime) = media_type else {
        debug!(filename = %attachment.filename, "skipping non-image attachment");
        return None;
    };
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    if !mime.starts_with("image/") {
        debug!(filename = %attachment.filename, mime = %mime, "skipping non-image attachment");
        return None;
    }

    if u64::from(attachment.size) > MAX_SIZE {
        error!(filename = %attachment.filename, size = attachment.size, "image exceeds 10MB limit");
        return None;
    }

    let response = match HTTP_CLIENT.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => { error!(url = %url, error = %e, "download failed"); return None; }
    };
    if !response.status().is_success() {
        error!(url = %url, status = %response.status(), "HTTP error downloading image");
        return None;
    }
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => { error!(url = %url, error = %e, "read failed"); return None; }
    };

    // Defense-in-depth: verify actual download size
    if bytes.len() as u64 > MAX_SIZE {
        error!(filename = %attachment.filename, size = bytes.len(), "downloaded image exceeds limit");
        return None;
    }

    // Resize and compress
    let (output_bytes, output_mime) = match resize_and_compress(&bytes) {
        Ok(result) => result,
        Err(e) => {
            // Fallback: use original bytes but reject if too large for transport
            if bytes.len() > 1024 * 1024 {
                error!(filename = %attachment.filename, error = %e, size = bytes.len(), "resize failed and original too large, skipping");
                return None;
            }
            debug!(filename = %attachment.filename, error = %e, "resize failed, using original");
            (bytes.to_vec(), mime.to_string())
        }
    };

    debug!(
        filename = %attachment.filename,
        original_size = bytes.len(),
        compressed_size = output_bytes.len(),
        "image processed"
    );

    let encoded = BASE64.encode(&output_bytes);
    Some(ContentBlock::Image {
        media_type: output_mime,
        data: encoded,
    })
}

/// Resize image so longest side ≤ IMAGE_MAX_DIMENSION_PX, then encode as JPEG.
/// Returns (compressed_bytes, mime_type). GIFs are passed through unchanged
/// to preserve animation.
fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()?;

    let format = reader.format();

    // Pass through GIFs unchanged to preserve animation
    if format == Some(image::ImageFormat::Gif) {
        return Ok((raw.to_vec(), "image/gif".to_string()));
    }

    let img = reader.decode()?;
    let (w, h) = (img.width(), img.height());

    // Resize preserving aspect ratio: scale so longest side = 1200px
    let img = if w > IMAGE_MAX_DIMENSION_PX || h > IMAGE_MAX_DIMENSION_PX {
        let max_side = std::cmp::max(w, h);
        let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::from(max_side);
        let new_w = (f64::from(w) * ratio) as u32;
        let new_h = (f64::from(h) * ratio) as u32;
        img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // Encode as JPEG
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, IMAGE_JPEG_QUALITY);
    img.write_with_encoder(encoder)?;

    Ok((buf.into_inner(), "image/jpeg".to_string()))
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
            // Tool calls indexed by toolCallId. Vec preserves first-seen
            // order. We store id + title + state separately so a ToolDone
            // event that arrives without a refreshed title (claude-agent-acp's
            // update events don't always re-send the title field) can still
            // reuse the title we already learned from a prior
            // tool_call_update — only the icon flips 🔧 → ✅ / ❌. Rendering
            // happens on the fly in compose_display().
            let mut tool_lines: Vec<ToolEntry> = Vec::new();
            let current_msg_id = msg_id;

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            // Spawn edit-streaming task — only edits the single message, never sends new ones.
            // Long content is truncated during streaming; final multi-message split happens after.
            let edit_handle = {
                let ctx = ctx.clone();
                let mut buf_rx = buf_rx.clone();
                tokio::spawn(async move {
                    let mut last_content = String::new();
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        if buf_rx.has_changed().unwrap_or(false) {
                            let content = buf_rx.borrow_and_update().clone();
                            if content != last_content {
                                let display = if content.chars().count() > 1900 {
                                    let truncated = format::truncate_chars(&content, 1900);
                                    format!("{truncated}…")
                                } else {
                                    content.clone()
                                };
                                let _ = edit(&ctx, channel, msg_id, &display).await;
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
                        AcpEvent::ToolStart { id, title } if !title.is_empty() => {
                            reactions.set_tool(&title).await;
                            let title = sanitize_title(&title);
                            // Dedupe by toolCallId: replace if we've already
                            // seen this id, otherwise append a new entry.
                            // claude-agent-acp emits a placeholder title
                            // ("Terminal", "Edit", etc.) on the first event
                            // and refines it via tool_call_update; without
                            // dedup the placeholder and refined version
                            // appear as two separate orphaned lines.
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
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        AcpEvent::ToolDone { id, title, status } => {
                            reactions.set_thinking().await;
                            let new_state = if status == "completed" {
                                ToolState::Completed
                            } else {
                                ToolState::Failed
                            };
                            // Find by id (the title is unreliable — substring
                            // match against the placeholder "Terminal" would
                            // never find the refined entry). Preserve the
                            // existing title if the Done event omits it.
                            if let Some(slot) = tool_lines.iter_mut().find(|e| e.id == id) {
                                if !title.is_empty() {
                                    slot.title = sanitize_title(&title);
                                }
                                slot.state = new_state;
                            } else if !title.is_empty() {
                                // Done arrived without a prior Start (rare
                                // race) — record it so we still show
                                // something.
                                tool_lines.push(ToolEntry {
                                    id,
                                    title: sanitize_title(&title),
                                    state: new_state,
                                });
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

/// Flatten a tool-call title into a single line that's safe to render
/// inside Discord inline-code spans. Discord renders single-backtick
/// code on a single line only, so multi-line shell commands (heredocs,
/// `&&`-chained commands split across lines) appear truncated; we
/// collapse newlines to ` ; ` and rewrite embedded backticks so they
/// don't break the wrapping span.
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

fn compose_display(tool_lines: &[ToolEntry], text: &str) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() {
        for entry in tool_lines {
            out.push_str(&entry.render());
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


#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::new(width, height);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn large_image_resized_to_max_dimension() {
        let png = make_png(3000, 2000);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert!(result.width() <= IMAGE_MAX_DIMENSION_PX);
        assert!(result.height() <= IMAGE_MAX_DIMENSION_PX);
    }

    #[test]
    fn small_image_keeps_original_dimensions() {
        let png = make_png(800, 600);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 800);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn landscape_image_respects_aspect_ratio() {
        let png = make_png(4000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 1200);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn portrait_image_respects_aspect_ratio() {
        let png = make_png(2000, 4000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 600);
        assert_eq!(result.height(), 1200);
    }

    #[test]
    fn compressed_output_is_smaller_than_original() {
        let png = make_png(3000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        assert!(compressed.len() < png.len(), "compressed {} should be < original {}", compressed.len(), png.len());
    }

    #[test]
    fn gif_passes_through_unchanged() {
        // Minimal valid GIF89a (1x1 pixel)
        let gif: Vec<u8> = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, // logical screen descriptor
            0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, // image descriptor
            0x02, 0x02, 0x44, 0x01, 0x00, // image data
            0x3B, // trailer
        ];
        let (output, mime) = resize_and_compress(&gif).unwrap();

        assert_eq!(mime, "image/gif");
        assert_eq!(output, gif);
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = vec![0x00, 0x01, 0x02, 0x03];
        assert!(resize_and_compress(&garbage).is_err());
    }
}

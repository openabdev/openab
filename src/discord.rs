use crate::acp::ContentBlock;
use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::config::{AllowBots, AllowUsers, SttConfig};
use crate::format;
use crate::media;
use async_trait::async_trait;
use std::sync::LazyLock;
use serenity::builder::CreateThread;
use serenity::http::Http;
use serenity::model::channel::{AutoArchiveDuration, Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, MessageId, UserId};
use serenity::model::user::User;
use serenity::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tracing::{debug, error, info};

/// Hard cap on consecutive bot messages in a channel or thread.
/// Prevents runaway loops between multiple bots in "all" mode.
const MAX_CONSECUTIVE_BOT_TURNS: u8 = 10;

/// Maximum entries in the participation cache before eviction.
const PARTICIPATION_CACHE_MAX: usize = 1000;

// --- DiscordAdapter: implements ChatAdapter for Discord via serenity ---

pub struct DiscordAdapter {
    http: Arc<Http>,
}

impl DiscordAdapter {
    pub fn new(http: Arc<Http>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ChatAdapter for DiscordAdapter {
    fn platform(&self) -> &'static str {
        "discord"
    }

    fn message_limit(&self) -> usize {
        2000
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> anyhow::Result<MessageRef> {
        let ch_id: u64 = channel.channel_id.parse()?;
        let msg = ChannelId::new(ch_id).say(&self.http, content).await?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: msg.id.to_string(),
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        title: &str,
    ) -> anyhow::Result<ChannelRef> {
        let ch_id: u64 = channel.channel_id.parse()?;
        let msg_id: u64 = trigger_msg.message_id.parse()?;
        let thread = ChannelId::new(ch_id)
            .create_thread_from_message(
                &self.http,
                MessageId::new(msg_id),
                CreateThread::new(title).auto_archive_duration(AutoArchiveDuration::OneDay),
            )
            .await?;
        Ok(ChannelRef {
            platform: "discord".into(),
            channel_id: thread.id.to_string(),
            thread_id: None,
            parent_id: Some(channel.channel_id.clone()),
        })
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> anyhow::Result<()> {
        let ch_id: u64 = msg.channel.channel_id.parse()?;
        let msg_id: u64 = msg.message_id.parse()?;
        self.http
            .create_reaction(
                ChannelId::new(ch_id),
                MessageId::new(msg_id),
                &ReactionType::Unicode(emoji.to_string()),
            )
            .await?;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> anyhow::Result<()> {
        let ch_id: u64 = msg.channel.channel_id.parse()?;
        let msg_id: u64 = msg.message_id.parse()?;
        self.http
            .delete_reaction_me(
                ChannelId::new(ch_id),
                MessageId::new(msg_id),
                &ReactionType::Unicode(emoji.to_string()),
            )
            .await?;
        Ok(())
    }
}

// --- Handler: serenity EventHandler that delegates to AdapterRouter ---

pub struct Handler {
    pub router: Arc<AdapterRouter>,
    pub allowed_channels: HashSet<u64>,
    pub allowed_users: HashSet<u64>,
    pub stt_config: SttConfig,
    pub adapter: OnceLock<Arc<dyn ChatAdapter>>,
    pub allow_bot_messages: AllowBots,
    pub trusted_bot_ids: HashSet<u64>,
    pub allow_user_messages: AllowUsers,
    /// Positive-only cache: thread channel_id → cached_at for threads where bot has participated.
    pub participated_threads: tokio::sync::Mutex<HashMap<String, tokio::time::Instant>>,
    /// TTL for participation cache entries (from pool.session_ttl_hours).
    pub session_ttl: std::time::Duration,
}

impl Handler {
    /// Check if the bot has participated in a Discord thread.
    /// Returns true if any message in the thread is from the bot.
    /// Fail-closed: returns false on API error.
    /// Only caches positive results (participation is irreversible).
    async fn bot_participated_in_thread(
        &self,
        http: &Http,
        channel_id: ChannelId,
        bot_id: UserId,
    ) -> bool {
        let key = channel_id.to_string();

        // Check positive cache
        {
            let cache = self.participated_threads.lock().await;
            if let Some(cached_at) = cache.get(&key) {
                if cached_at.elapsed() < self.session_ttl {
                    return true;
                }
            }
        }

        // Fetch recent messages and check if bot posted any
        let messages = match channel_id
            .messages(http, serenity::builder::GetMessages::new().limit(200))
            .await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!(
                    channel_id = %channel_id,
                    error = %e,
                    "failed to fetch thread messages for participation check, rejecting (fail-closed)"
                );
                return false;
            }
        };

        let involved = messages.iter().any(|m| m.author.id == bot_id);

        if involved {
            let mut cache = self.participated_threads.lock().await;
            cache.insert(key, tokio::time::Instant::now());

            // Evict if over capacity
            if cache.len() > PARTICIPATION_CACHE_MAX {
                cache.retain(|_, ts| ts.elapsed() < self.session_ttl);
                if cache.len() > PARTICIPATION_CACHE_MAX {
                    let mut entries: Vec<_> = cache.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    entries.sort_by_key(|(_, ts)| *ts);
                    let evict_count = entries.len() / 2;
                    for (k, _) in entries.into_iter().take(evict_count) {
                        cache.remove(&k);
                    }
                }
            }
        }

        involved
    }
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        let bot_id = ctx.cache.current_user().id;

        // Always ignore own messages
        if msg.author.id == bot_id {
            return;
        }

        let adapter = self.adapter.get_or_init(|| {
            Arc::new(DiscordAdapter::new(ctx.http.clone()))
        }).clone();

        let channel_id = msg.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);

        let is_mentioned = msg.mentions_user_id(bot_id)
            || msg.content.contains(&format!("<@{}>", bot_id));

        // Bot message gating (from upstream #321)
        if msg.author.bot {
            match self.allow_bot_messages {
                AllowBots::Off => return,
                AllowBots::Mentions => if !is_mentioned { return; },
                AllowBots::All => {
                    let cap = MAX_CONSECUTIVE_BOT_TURNS as usize;
                    let history = ctx.cache.channel_messages(msg.channel_id)
                        .map(|msgs| {
                            let mut recent: Vec<_> = msgs.iter()
                                .filter(|(mid, _)| **mid < msg.id)
                                .map(|(_, m)| m.clone())
                                .collect();
                            recent.sort_unstable_by_key(|m| std::cmp::Reverse(m.id));
                            recent.truncate(cap);
                            recent
                        })
                        .filter(|msgs| !msgs.is_empty());

                    let recent = if let Some(cached) = history {
                        cached
                    } else {
                        match msg.channel_id
                            .messages(&ctx.http, serenity::builder::GetMessages::new().before(msg.id).limit(MAX_CONSECUTIVE_BOT_TURNS))
                            .await
                        {
                            Ok(msgs) => msgs,
                            Err(e) => {
                                tracing::warn!(channel_id = %msg.channel_id, error = %e, "failed to fetch history for bot turn cap, rejecting (fail-closed)");
                                return;
                            }
                        }
                    };

                    let consecutive_bot = recent.iter()
                        .take_while(|m| m.author.bot && m.author.id != bot_id)
                        .count();
                    if consecutive_bot >= cap {
                        tracing::warn!(channel_id = %msg.channel_id, cap, "bot turn cap reached, ignoring");
                        return;
                    }
                },
            }

            if !self.trusted_bot_ids.is_empty() && !self.trusted_bot_ids.contains(&msg.author.id.get()) {
                tracing::debug!(bot_id = %msg.author.id, "bot not in trusted_bot_ids, ignoring");
                return;
            }
        }

        // Thread detection: check if the message is in a thread whose parent
        // is an allowed channel, and whether the bot owns that thread.
        let (in_thread, bot_owns_thread) = if !in_allowed_channel {
            match msg.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => {
                    let parent_allowed = gc
                        .parent_id
                        .is_some_and(|pid| self.allowed_channels.contains(&pid.get()));
                    let owned = gc.owner_id.is_some_and(|oid| oid == bot_id);
                    tracing::debug!(
                        channel_id = %msg.channel_id,
                        parent_id = ?gc.parent_id,
                        owner_id = ?gc.owner_id,
                        parent_allowed,
                        bot_owns = owned,
                        "thread check"
                    );
                    (parent_allowed, owned)
                }
                Ok(other) => {
                    tracing::debug!(channel_id = %msg.channel_id, kind = ?other, "not a guild channel");
                    (false, false)
                }
                Err(e) => {
                    tracing::debug!(channel_id = %msg.channel_id, error = %e, "to_channel failed");
                    (false, false)
                }
            }
        } else {
            (false, false)
        };

        if !in_allowed_channel && !in_thread {
            return;
        }

        // User message gating (mirrors Slack's AllowUsers logic).
        // Mentions: always require @mention, even in bot's own threads.
        // Involved (default): skip @mention if the bot owns the thread
        //   (Option A) OR has previously posted in it (Option B).
        if !is_mentioned {
            match self.allow_user_messages {
                AllowUsers::Mentions => return,
                AllowUsers::Involved => {
                    if !in_thread {
                        return;
                    }
                    let involved = bot_owns_thread
                        || self
                            .bot_participated_in_thread(&ctx.http, msg.channel_id, bot_id)
                            .await;
                    if !involved {
                        tracing::debug!(channel_id = %msg.channel_id, "bot not involved in thread, ignoring");
                        return;
                    }
                }
            }
        }

        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&msg.author.id.get()) {
            tracing::info!(user_id = %msg.author.id, "denied user, ignoring");
            let msg_ref = discord_msg_ref(&msg);
            let _ = adapter.add_reaction(&msg_ref, "🚫").await;
            return;
        }

        let prompt = if is_mentioned {
            resolve_mentions(&msg.content, bot_id, &msg.mentions)
        } else {
            msg.content.trim().to_string()
        };

        // No text and no attachments → skip
        if prompt.is_empty() && msg.attachments.is_empty() {
            return;
        }

        let display_name = msg
            .member
            .as_ref()
            .and_then(|m| m.nick.as_ref())
            .unwrap_or(&msg.author.name);
        let sender = SenderContext {
            schema: "openab.sender.v1".into(),
            sender_id: msg.author.id.to_string(),
            sender_name: msg.author.name.clone(),
            display_name: display_name.to_string(),
            channel: "discord".into(),
            channel_id: msg.channel_id.to_string(),
            thread_id: None,
            is_bot: msg.author.bot,
        };

        // Build extra content blocks from attachments (images, audio)
        let mut extra_blocks = Vec::new();
        for attachment in &msg.attachments {
            let mime = attachment.content_type.as_deref().unwrap_or("");
            if media::is_audio_mime(mime) {
                if self.stt_config.enabled {
                    let mime_clean = mime.split(';').next().unwrap_or(mime).trim();
                    if let Some(transcript) = media::download_and_transcribe(
                        &attachment.url,
                        &attachment.filename,
                        mime_clean,
                        u64::from(attachment.size),
                        &self.stt_config,
                        None,
                    ).await {
                        debug!(filename = %attachment.filename, chars = transcript.len(), "voice transcript injected");
                        extra_blocks.insert(0, ContentBlock::Text {
                            text: format!("[Voice message transcript]: {transcript}"),
                        });
                    }
                } else {
                    tracing::warn!(filename = %attachment.filename, "skipping audio attachment (STT disabled)");
                    let msg_ref = discord_msg_ref(&msg);
                    let _ = adapter.add_reaction(&msg_ref, "🎤").await;
                }
            } else if let Some(block) = media::download_and_encode_image(
                &attachment.url,
                attachment.content_type.as_deref(),
                &attachment.filename,
                u64::from(attachment.size),
                None,
            ).await {
                debug!(url = %attachment.url, filename = %attachment.filename, "adding image attachment");
                extra_blocks.push(block);
            }
        }

        tracing::debug!(
            num_extra_blocks = extra_blocks.len(),
            num_attachments = msg.attachments.len(),
            in_thread,
            "processing"
        );

        let thread_channel = if in_thread {
            ChannelRef {
                platform: "discord".into(),
                channel_id: msg.channel_id.get().to_string(),
                thread_id: None,
                parent_id: None,
            }
        } else {
            match get_or_create_thread(&ctx, &adapter, &msg, &prompt).await {
                Ok(ch) => ch,
                Err(e) => {
                    error!("failed to create thread: {e}");
                    return;
                }
            }
        };

        let trigger_msg = discord_msg_ref(&msg);

        let router = self.router.clone();
        tokio::spawn(async move {
            if let Err(e) = router
                .handle_message(&adapter, &thread_channel, &sender, &prompt, extra_blocks, &trigger_msg)
                .await
            {
                error!("handle_message error: {e}");
            }
        });
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "discord bot connected");
    }
}

// --- Discord-specific helpers ---

fn discord_msg_ref(msg: &Message) -> MessageRef {
    MessageRef {
        channel: ChannelRef {
            platform: "discord".into(),
            channel_id: msg.channel_id.get().to_string(),
            thread_id: None,
            parent_id: None,
        },
        message_id: msg.id.to_string(),
    }
}

async fn get_or_create_thread(
    ctx: &Context,
    adapter: &Arc<dyn ChatAdapter>,
    msg: &Message,
    prompt: &str,
) -> anyhow::Result<ChannelRef> {
    let channel = msg.channel_id.to_channel(&ctx.http).await?;
    if let serenity::model::channel::Channel::Guild(ref gc) = channel {
        if gc.thread_metadata.is_some() {
            return Ok(ChannelRef {
                platform: "discord".into(),
                channel_id: msg.channel_id.get().to_string(),
                thread_id: None,
                parent_id: None,
            });
        }
    }

    let thread_name = format::shorten_thread_name(prompt);
    let parent = ChannelRef {
        platform: "discord".into(),
        channel_id: msg.channel_id.get().to_string(),
        thread_id: None,
        parent_id: None,
    };
    let trigger_ref = discord_msg_ref(msg);
    adapter.create_thread(&parent, &trigger_ref, &thread_name).await
}

static ROLE_MENTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<@&\d+>").unwrap()
});
static USER_MENTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<@!?\d+>").unwrap()
});

fn resolve_mentions(content: &str, bot_id: UserId, mentions: &[User]) -> String {
    // 1. Strip the bot's own trigger mention
    let mut out = content
        .replace(&format!("<@{}>", bot_id), "")
        .replace(&format!("<@!{}>", bot_id), "");
    // 2. Resolve known user mentions to @DisplayName
    for user in mentions {
        if user.id == bot_id {
            continue;
        }
        let label = user.global_name.as_deref().unwrap_or(&user.name);
        let display = format!("@{}", label);
        out = out
            .replace(&format!("<@{}>", user.id), &display)
            .replace(&format!("<@!{}>", user.id), &display);
    }
    // 3. Fallback: replace any remaining unresolved mentions
    let out = ROLE_MENTION_RE.replace_all(&out, "@(role)");
    let out = USER_MENTION_RE.replace_all(&out, "@(user)").to_string();
    out.trim().to_string()
}

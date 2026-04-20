use crate::acp::ContentBlock;
use crate::acp::protocol::ConfigOption;
use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::config::{AllowBots, AllowUsers, SttConfig};
use crate::format;
use crate::media;
use async_trait::async_trait;
use std::sync::LazyLock;
use serenity::builder::{CreateActionRow, CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, CreateThread};
use serenity::http::Http;
use serenity::model::application::{ComponentInteractionDataKind, Interaction};
use serenity::model::channel::{AutoArchiveDuration, Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, MessageId, UserId};
use serenity::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tracing::{debug, error, info};

/// Hard cap on consecutive bot messages in a channel or thread.
/// Prevents runaway loops between multiple bots in "all" mode.
const MAX_CONSECUTIVE_BOT_TURNS: u8 = 10;

/// Absolute per-thread cap on bot turns. Cannot be overridden by config or human intervention.
const HARD_BOT_TURN_LIMIT: u32 = 100;

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
    pub allow_all_channels: bool,
    pub allow_all_users: bool,
    pub allowed_channels: HashSet<u64>,
    pub allowed_users: HashSet<u64>,
    pub stt_config: SttConfig,
    pub adapter: OnceLock<Arc<dyn ChatAdapter>>,
    pub allow_bot_messages: AllowBots,
    pub trusted_bot_ids: HashSet<u64>,
    pub allow_user_messages: AllowUsers,
    /// Positive-only cache: thread channel_id → cached_at for threads where bot has participated.
    pub participated_threads: tokio::sync::Mutex<HashMap<String, tokio::time::Instant>>,
    /// Positive-only cache: thread channel_id → cached_at for threads where other bots have posted.
    /// Like participation, a thread becoming multi-bot is irreversible (bot messages don't disappear).
    pub multibot_threads: tokio::sync::Mutex<HashMap<String, tokio::time::Instant>>,
    /// TTL for participation cache entries (from pool.session_ttl_hours).
    pub session_ttl: std::time::Duration,
    /// Configurable soft limit on bot turns per thread (reset by human message).
    pub max_bot_turns: u32,
    /// Per-thread bot turn tracker. Both counters reset on human msg.
    pub bot_turns: tokio::sync::Mutex<BotTurnTracker>,
}

impl Handler {
    /// Check if the bot has participated in a Discord thread, and whether
    /// other bots have also posted in it.
    /// Returns `(involved, other_bot_present)`.
    /// Fail-closed: returns `(false, false)` on API error.
    /// Caches positive results only (both participation and multi-bot status are irreversible).
    async fn bot_participated_in_thread(
        &self,
        http: &Http,
        channel_id: ChannelId,
        bot_id: UserId,
    ) -> (bool, bool) {
        let key = channel_id.to_string();

        // Check positive caches
        let cached_involved = {
            let cache = self.participated_threads.lock().await;
            cache.get(&key).is_some_and(|ts| ts.elapsed() < self.session_ttl)
        };
        let cached_multibot = {
            let cache = self.multibot_threads.lock().await;
            cache.get(&key).is_some_and(|ts| ts.elapsed() < self.session_ttl)
        };

        // Both cached → skip fetch entirely
        // With early detection from msg.author, multibot_threads is populated
        // eagerly — no need to fetch just to check for other bots.
        if cached_involved {
            return (true, cached_multibot);
        }

        // Fetch recent messages
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
                return (false, false);
            }
        };

        let involved = cached_involved || messages.iter().any(|m| m.author.id == bot_id);
        let other_bot_present = cached_multibot || messages.iter().any(|m| m.author.bot && m.author.id != bot_id);

        if involved && !cached_involved {
            let mut cache = self.participated_threads.lock().await;
            cache.insert(key.clone(), tokio::time::Instant::now());

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

        if other_bot_present && !cached_multibot {
            let mut cache = self.multibot_threads.lock().await;
            cache.insert(key, tokio::time::Instant::now());

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

        (involved, other_bot_present)
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
            self.allow_all_channels || self.allowed_channels.contains(&channel_id);

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
                    let parent_allowed = self.allow_all_channels || gc
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

        // Early multibot detection: if the current message is from another bot,
        // this thread is multi-bot. Cache it now — no fetch needed.
        if in_thread && msg.author.bot && msg.author.id != bot_id {
            let key = msg.channel_id.to_string();
            let mut cache = self.multibot_threads.lock().await;
            cache.entry(key).or_insert_with(tokio::time::Instant::now);
        }

        // User message gating (mirrors Slack's AllowUsers logic).
        // Mentions: always require @mention, even in bot's own threads.
        // Involved (default): skip @mention if the bot owns the thread
        //   (Option A) OR has previously posted in it (Option B).
        // MultibotMentions: same as Involved, but if other bots are also
        //   in the thread, require @mention to avoid all bots responding.
        if !is_mentioned {
            match self.allow_user_messages {
                AllowUsers::Mentions => return,
                AllowUsers::Involved => {
                    if !in_thread {
                        return;
                    }
                    let (involved, _) = if bot_owns_thread {
                        (true, false) // other_bot_present not needed for Involved mode
                    } else {
                        self.bot_participated_in_thread(&ctx.http, msg.channel_id, bot_id)
                            .await
                    };
                    if !involved {
                        tracing::debug!(channel_id = %msg.channel_id, "bot not involved in thread, ignoring");
                        return;
                    }
                }
                AllowUsers::MultibotMentions => {
                    if !in_thread {
                        return;
                    }
                    let (involved, other_bot) = if bot_owns_thread {
                        // Still need to check for other bots
                        let (_, other) = self
                            .bot_participated_in_thread(&ctx.http, msg.channel_id, bot_id)
                            .await;
                        (true, other)
                    } else {
                        self.bot_participated_in_thread(&ctx.http, msg.channel_id, bot_id)
                            .await
                    };
                    if !involved {
                        tracing::debug!(channel_id = %msg.channel_id, "bot not involved in thread, ignoring");
                        return;
                    }
                    if other_bot {
                        tracing::debug!(channel_id = %msg.channel_id, "multi-bot thread, requiring @mention");
                        return;
                    }
                }
            }
        }

        if !self.allow_all_users && !self.allowed_users.contains(&msg.author.id.get()) {
            tracing::info!(user_id = %msg.author.id, "denied user, ignoring");
            let msg_ref = discord_msg_ref(&msg);
            let _ = adapter.add_reaction(&msg_ref, "🚫").await;
            return;
        }

        let prompt = resolve_mentions(&msg.content, bot_id);

        // Bot turn limiting: track consecutive bot turns per thread.
        // Placed after all gating so only messages that will actually be
        // processed count toward the limit.
        // Human message resets both soft and hard counters.
        {
            let thread_key = msg.channel_id.to_string();
            let mut tracker = self.bot_turns.lock().await;
            if msg.author.bot {
                match tracker.on_bot_message(&thread_key) {
                    TurnResult::HardLimit => {
                        tracing::warn!(channel_id = %msg.channel_id, "hard bot turn limit reached");
                        let _ = msg.channel_id.say(
                            &ctx.http,
                            format!("🛑 Hard limit reached ({HARD_BOT_TURN_LIMIT}). Bot-to-bot conversation in this thread has been permanently stopped."),
                        ).await;
                        return;
                    }
                    TurnResult::SoftLimit(n) => {
                        tracing::info!(channel_id = %msg.channel_id, turns = n, max = self.max_bot_turns, "soft bot turn limit reached");
                        let _ = msg.channel_id.say(
                            &ctx.http,
                            format!("⚠️ Bot turn limit reached ({n}/{}). A human must reply in this thread to continue bot-to-bot conversation.", self.max_bot_turns),
                        ).await;
                        return;
                    }
                    TurnResult::Ok => {}
                }
            } else {
                tracker.on_human_message(&thread_key);
            }
        }

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
            let sender_json = serde_json::to_string(&sender).unwrap();
            if let Err(e) = router
                .handle_message(&adapter, &thread_channel, &sender_json, &prompt, extra_blocks, &trigger_msg)
                .await
            {
                error!("handle_message error: {e}");
            }
        });
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "discord bot connected");

        // Register /model slash command to all guilds the bot is in
        for guild in &ready.guilds {
            let guild_id = guild.id;
            if let Err(e) = guild_id
                .set_commands(
                    &ctx.http,
                    vec![
                        CreateCommand::new("models")
                            .description("Select the AI model for this session"),
                        CreateCommand::new("agents")
                            .description("Select the agent mode for this session"),
                        CreateCommand::new("cancel")
                            .description("Cancel the current operation"),
                    ],
                )
                .await
            {
                tracing::warn!(%guild_id, error = %e, "failed to register slash commands");
            } else {
                info!(%guild_id, "registered slash commands");
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(cmd) if cmd.data.name == "models" => {
                self.handle_config_command(&ctx, &cmd, "model", "model").await;
            }
            Interaction::Command(cmd) if cmd.data.name == "agents" => {
                self.handle_config_command(&ctx, &cmd, "agent", "agent").await;
            }
            Interaction::Command(cmd) if cmd.data.name == "cancel" => {
                self.handle_cancel_command(&ctx, &cmd).await;
            }
            Interaction::Component(comp) if comp.data.custom_id.starts_with("acp_config_") => {
                self.handle_config_select(&ctx, &comp).await;
            }
            _ => {}
        }
    }
}


// --- Slash command & interaction handlers ---

impl Handler {
    /// Build a Discord select menu from ACP configOptions with the given category.
    fn build_config_select(options: &[ConfigOption], category: &str) -> Option<CreateSelectMenu> {
        let opt = options.iter().find(|o| o.category.as_deref() == Some(category))?;
        let menu_options: Vec<CreateSelectMenuOption> = opt
            .options
            .iter()
            .map(|o| {
                let mut item = CreateSelectMenuOption::new(&o.name, &o.value);
                if let Some(desc) = &o.description {
                    item = item.description(desc);
                }
                if o.value == opt.current_value {
                    item = item.default_selection(true);
                }
                item
            })
            .collect();

        if menu_options.is_empty() {
            return None;
        }

        Some(
            CreateSelectMenu::new(
                format!("acp_config_{}", opt.id),
                CreateSelectMenuKind::String { options: menu_options },
            )
            .placeholder(format!("Current: {}", opt.options.iter()
                .find(|o| o.value == opt.current_value)
                .map(|o| o.name.as_str())
                .unwrap_or(&opt.current_value)))
        )
    }

    async fn handle_config_command(
        &self,
        ctx: &Context,
        cmd: &serenity::model::application::CommandInteraction,
        category: &str,
        label: &str,
    ) {
        let thread_key = format!("discord:{}", cmd.channel_id.get());
        let config_options = self.router.pool().get_config_options(&thread_key).await;
        let select = Self::build_config_select(&config_options, category);

        let response = match select {
            Some(menu) => CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("🔧 Select a {label}:"))
                    .components(vec![CreateActionRow::SelectMenu(menu)])
                    .ephemeral(true),
            ),
            None => CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("⚠️ No {label} options available. Start a conversation first by @mentioning the bot."))
                    .ephemeral(true),
            ),
        };

        if let Err(e) = cmd.create_response(&ctx.http, response).await {
            tracing::error!(error = %e, category, "failed to respond to slash command");
        }
    }

    async fn handle_cancel_command(
        &self,
        ctx: &Context,
        cmd: &serenity::model::application::CommandInteraction,
    ) {
        let thread_key = format!("discord:{}", cmd.channel_id.get());
        let result = self.router.pool().cancel_session(&thread_key).await;

        let msg = match result {
            Ok(()) => "🛑 Cancel signal sent.".to_string(),
            Err(e) => format!("⚠️ {e}"),
        };

        let response = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().content(msg).ephemeral(true),
        );
        if let Err(e) = cmd.create_response(&ctx.http, response).await {
            tracing::error!(error = %e, "failed to respond to /cancel command");
        }
    }

    async fn handle_config_select(
        &self,
        ctx: &Context,
        comp: &serenity::model::application::ComponentInteraction,
    ) {
        let config_id = comp
            .data
            .custom_id
            .strip_prefix("acp_config_")
            .unwrap_or("")
            .to_string();

        if config_id.is_empty() {
            return;
        }

        let selected_value = match &comp.data.kind {
            ComponentInteractionDataKind::StringSelect { values } => {
                match values.first() {
                    Some(v) => v.clone(),
                    None => return,
                }
            }
            _ => return,
        };

        let thread_key = format!("discord:{}", comp.channel_id.get());

        let result = self
            .router
            .pool()
            .set_config_option(&thread_key, &config_id, &selected_value)
            .await;

        let response_msg = match result {
            Ok(updated_options) => {
                let display_name = updated_options
                    .iter()
                    .find(|o| o.id == config_id)
                    .and_then(|o| o.options.iter().find(|v| v.value == selected_value))
                    .map(|v| v.name.as_str())
                    .unwrap_or(&selected_value);
                format!("✅ Switched to **{}**", display_name)
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to set config option");
                format!("❌ Failed to switch: {}", e)
            }
        };

        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new().content(response_msg).components(vec![]),
        );

        if let Err(e) = comp.create_response(&ctx.http, response).await {
            tracing::error!(error = %e, "failed to respond to config select");
        }
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

// --- Bot turn tracking ---

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TurnResult {
    Ok,
    SoftLimit(u32),
    HardLimit,
}

pub(crate) struct BotTurnTracker {
    soft_limit: u32,
    counts: HashMap<String, (u32, u32)>,
}

impl BotTurnTracker {
    pub fn new(soft_limit: u32) -> Self {
        Self { soft_limit, counts: HashMap::new() }
    }

    pub fn on_bot_message(&mut self, thread_id: &str) -> TurnResult {
        let (soft, hard) = self.counts.entry(thread_id.to_string()).or_insert((0, 0));
        *soft += 1;
        *hard += 1;
        if *hard >= HARD_BOT_TURN_LIMIT {
            TurnResult::HardLimit
        } else if *soft >= self.soft_limit {
            TurnResult::SoftLimit(*soft)
        } else {
            TurnResult::Ok
        }
    }

    pub fn on_human_message(&mut self, thread_id: &str) {
        if let Some((soft, hard)) = self.counts.get_mut(thread_id) {
            *soft = 0;
            *hard = 0;
        }
    }
}

static ROLE_MENTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<@&\d+>").unwrap()
});

fn resolve_mentions(content: &str, bot_id: UserId) -> String {
    // 1. Strip the bot's own trigger mention
    let out = content
        .replace(&format!("<@{}>", bot_id), "")
        .replace(&format!("<@!{}>", bot_id), "");
    // 2. Other user mentions: keep <@UID> as-is so the LLM can mention back
    // 3. Fallback: replace role mentions only (user mentions are preserved)
    let out = ROLE_MENTION_RE.replace_all(&out, "@(role)").to_string();
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_turns_increment() {
        let mut t = BotTurnTracker::new(5);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
    }

    #[test]
    fn soft_limit_triggers() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
    }

    #[test]
    fn human_resets_both_counters() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        t.on_human_message("t1");
        // Both reset — can do 2 more before soft limit
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
    }

    #[test]
    fn hard_limit_triggers() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::HardLimit);
    }

    #[test]
    fn hard_limit_resets_on_human() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        t.on_human_message("t1");
        // Hard counter reset — can go again
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
    }

    #[test]
    fn hard_before_soft_when_equal() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        // soft == hard == HARD_BOT_TURN_LIMIT → hard wins
        assert_eq!(t.on_bot_message("t1"), TurnResult::HardLimit);
    }

    #[test]
    fn threads_are_independent() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
        // t2 is unaffected
        assert_eq!(t.on_bot_message("t2"), TurnResult::Ok);
    }

    #[test]
    fn human_on_unknown_thread_is_noop() {
        let mut t = BotTurnTracker::new(5);
        t.on_human_message("unknown"); // should not panic
    }
}

use crate::acp::ContentBlock;
use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::config::{AllowBots, AllowUsers, SttConfig};
use crate::media;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

const SLACK_API: &str = "https://slack.com/api";

/// Map Unicode emoji to Slack short names for reactions API.
/// Only covers the default `[reactions.emojis]` set. Custom emoji configured
/// outside this map will fall back to `grey_question`.
fn unicode_to_slack_emoji(unicode: &str) -> &str {
    match unicode {
        "👀" => "eyes",
        "🤔" => "thinking_face",
        "🔥" => "fire",
        "👨\u{200d}💻" => "technologist",
        "⚡" => "zap",
        "🆗" => "ok",
        "😱" => "scream",
        "🚫" => "no_entry_sign",
        "😊" => "blush",
        "😎" => "sunglasses",
        "🫡" => "saluting_face",
        "🤓" => "nerd_face",
        "😏" => "smirk",
        "✌\u{fe0f}" => "v",
        "💪" => "muscle",
        "🦾" => "mechanical_arm",
        "🥱" => "yawning_face",
        "😨" => "fearful",
        "✅" => "white_check_mark",
        "❌" => "x",
        "🔧" => "wrench",
        "🎤" => "microphone",
        _ => "grey_question",
    }
}

// --- SlackAdapter: implements ChatAdapter for Slack ---

/// TTL for cached user display names (5 minutes).
const USER_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Maximum entries in the participation cache before eviction.
const PARTICIPATION_CACHE_MAX: usize = 1000;

pub struct SlackAdapter {
    client: reqwest::Client,
    bot_token: String,
    bot_user_id: tokio::sync::OnceCell<String>,
    user_cache: tokio::sync::Mutex<HashMap<String, (String, tokio::time::Instant)>>,
    /// Cache: Bot ID (B...) → Bot User ID (U...) for trusted_bot_ids matching.
    bot_id_cache: tokio::sync::Mutex<HashMap<String, String>>,
    /// Positive-only cache: thread_ts → cached_at for threads where bot has participated.
    participated_threads: tokio::sync::Mutex<HashMap<String, tokio::time::Instant>>,
    /// TTL for participation cache entries (matches session_ttl_hours from config).
    session_ttl: std::time::Duration,
    /// Controls streaming behavior: Off → streaming edit, Mentions/All → send-once.
    allow_bot_messages: AllowBots,
}

impl SlackAdapter {
    pub fn new(bot_token: String, session_ttl: std::time::Duration, allow_bot_messages: AllowBots) -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token,
            bot_user_id: tokio::sync::OnceCell::new(),
            user_cache: tokio::sync::Mutex::new(HashMap::new()),
            bot_id_cache: tokio::sync::Mutex::new(HashMap::new()),
            participated_threads: tokio::sync::Mutex::new(HashMap::new()),
            session_ttl,
            allow_bot_messages,
        }
    }

    /// Get the bot's own Slack user ID (cached after first call).
    async fn get_bot_user_id(&self) -> Option<&str> {
        self.bot_user_id.get_or_try_init(|| async {
            let resp = self.api_post("auth.test", serde_json::json!({})).await
                .map_err(|e| anyhow!("auth.test failed: {e}"))?;
            resp["user_id"]
                .as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("no user_id in auth.test response"))
        }).await.ok().map(|s| s.as_str())
    }

    async fn api_post(&self, method: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .client
            .post(format!("{SLACK_API}/{method}"))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        if json["ok"].as_bool() != Some(true) {
            let err = json["error"].as_str().unwrap_or("unknown error");
            return Err(anyhow!("Slack API {method}: {err}"));
        }
        Ok(json)
    }

    /// Call a Slack API method using GET with query parameters.
    /// Required for read methods like conversations.replies that don't accept JSON body.
    async fn api_get(&self, method: &str, params: &[(&str, &str)]) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(format!("{SLACK_API}/{method}"))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .query(params)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        if json["ok"].as_bool() != Some(true) {
            let err = json["error"].as_str().unwrap_or("unknown error");
            return Err(anyhow!("Slack API {method}: {err}"));
        }
        Ok(json)
    }

    /// Resolve a Slack user ID to display name via users.info API.
    /// Results are cached for 5 minutes to avoid hitting Slack rate limits.
    async fn resolve_user_name(&self, user_id: &str) -> Option<String> {
        // Check cache first
        {
            let cache = self.user_cache.lock().await;
            if let Some((name, ts)) = cache.get(user_id) {
                if ts.elapsed() < USER_CACHE_TTL {
                    return Some(name.clone());
                }
            }
        }

        let resp = self
            .api_post(
                "users.info",
                serde_json::json!({ "user": user_id }),
            )
            .await
            .ok()?;
        let user = resp.get("user")?;
        let profile = user.get("profile")?;
        let display = profile
            .get("display_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let real = profile
            .get("real_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let name = user
            .get("name")
            .and_then(|v| v.as_str());
        let resolved = display.or(real).or(name)?.to_string();

        // Cache the result
        self.user_cache.lock().await.insert(
            user_id.to_string(),
            (resolved.clone(), tokio::time::Instant::now()),
        );

        Some(resolved)
    }

    /// Resolve a Bot ID (B...) to Bot User ID (U...) via bots.info API.
    /// Cached permanently (bot IDs don't change).
    async fn resolve_bot_user_id(&self, bot_id: &str) -> Option<String> {
        {
            let cache = self.bot_id_cache.lock().await;
            if let Some(user_id) = cache.get(bot_id) {
                return Some(user_id.clone());
            }
        }

        let resp = self
            .api_post("bots.info", serde_json::json!({ "bot": bot_id }))
            .await
            .ok()?;
        let user_id = resp.get("bot")?
            .get("user_id")?
            .as_str()?
            .to_string();

        self.bot_id_cache.lock().await.insert(
            bot_id.to_string(),
            user_id.clone(),
        );

        Some(user_id)
    }

    /// Check if the bot has participated in a Slack thread.
    /// Returns true if: parent message @mentions the bot, OR any message in thread is from the bot.
    /// Fail-closed: returns false on API error (consistent with Discord's approach).
    /// Only caches positive results (involved=true is irreversible).
    async fn bot_participated_in_thread(&self, channel: &str, thread_ts: &str) -> bool {
        // Check positive cache first
        {
            let cache = self.participated_threads.lock().await;
            if let Some(cached_at) = cache.get(thread_ts) {
                if cached_at.elapsed() < self.session_ttl {
                    return true;
                }
            }
        }

        let bot_id = match self.get_bot_user_id().await {
            Some(id) => id,
            None => {
                warn!("cannot resolve bot user ID, rejecting (fail-closed)");
                return false;
            }
        };

        let resp = self
            .api_get(
                "conversations.replies",
                &[
                    ("channel", channel),
                    ("ts", thread_ts),
                    ("limit", "200"),
                    ("inclusive", "true"),
                ],
            )
            .await;

        let json = match resp {
            Ok(json) => json,
            Err(e) => {
                warn!(channel, thread_ts, error = %e, "failed to fetch thread replies, rejecting (fail-closed)");
                return false;
            }
        };
        let Some(messages) = json["messages"].as_array() else { return false };

        // Check if parent message @mentions the bot
        let parent_mentions_bot = messages
            .first()
            .and_then(|m| m["text"].as_str())
            .is_some_and(|text| text.contains(&format!("<@{bot_id}>")));

        // Check if any message in thread is from the bot
        let bot_posted = messages.iter().any(|m| m["user"].as_str() == Some(bot_id));

        let involved = parent_mentions_bot || bot_posted;

        if involved {
            self.cache_participation(thread_ts).await;
        }

        involved
    }

    /// Insert a positive participation entry, enforcing cache bounds.
    async fn cache_participation(&self, thread_ts: &str) {
        let mut cache = self.participated_threads.lock().await;
        let now = tokio::time::Instant::now();

        cache.insert(thread_ts.to_string(), now);

        if cache.len() > PARTICIPATION_CACHE_MAX {
            // Evict expired entries first
            cache.retain(|_, ts| ts.elapsed() < self.session_ttl);

            // If still over, evict oldest half
            if cache.len() > PARTICIPATION_CACHE_MAX {
                let mut entries: Vec<_> = cache.iter().map(|(k, v)| (k.clone(), *v)).collect();
                entries.sort_by_key(|(_, ts)| *ts);
                let evict_count = entries.len() / 2;
                for (key, _) in entries.into_iter().take(evict_count) {
                    cache.remove(&key);
                }
            }
        }
    }
}

#[async_trait]
impl ChatAdapter for SlackAdapter {
    fn platform(&self) -> &'static str {
        "slack"
    }

    fn message_limit(&self) -> usize {
        4000
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let mrkdwn = markdown_to_mrkdwn(content);
        let mut body = serde_json::json!({
            "channel": channel.channel_id,
            "text": mrkdwn,
        });
        if let Some(thread_ts) = &channel.thread_id {
            body["thread_ts"] = serde_json::Value::String(thread_ts.clone());
        }
        let resp = self.api_post("chat.postMessage", body).await?;
        let ts = resp["ts"]
            .as_str()
            .ok_or_else(|| anyhow!("no ts in chat.postMessage response"))?;
        Ok(MessageRef {
            channel: ChannelRef {
                platform: "slack".into(),
                channel_id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
                parent_id: None,
            },
            message_id: ts.to_string(),
        })
    }


    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        _title: &str,
    ) -> Result<ChannelRef> {
        // Slack threads are implicit — posting with thread_ts creates/continues a thread.
        Ok(ChannelRef {
            platform: "slack".into(),
            channel_id: channel.channel_id.clone(),
            thread_id: Some(trigger_msg.message_id.clone()),
            parent_id: None,
        })
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let name = unicode_to_slack_emoji(emoji);
        match self.api_post(
            "reactions.add",
            serde_json::json!({
                "channel": msg.channel.channel_id,
                "timestamp": msg.message_id,
                "name": name,
            }),
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("already_reacted") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let name = unicode_to_slack_emoji(emoji);
        match self.api_post(
            "reactions.remove",
            serde_json::json!({
                "channel": msg.channel.channel_id,
                "timestamp": msg.message_id,
                "name": name,
            }),
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("no_reaction") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        let mrkdwn = markdown_to_mrkdwn(content);
        self.api_post(
            "chat.update",
            serde_json::json!({
                "channel": msg.channel.channel_id,
                "ts": msg.message_id,
                "text": mrkdwn,
            }),
        )
        .await?;
        Ok(())
    }

    fn use_streaming(&self) -> bool {
        self.allow_bot_messages == AllowBots::Off
    }
}

// --- Per-thread async queue (inspired by OpenClaw's KeyedAsyncQueue) ---

/// Serialize async work per key while allowing unrelated keys to run concurrently.
/// Same-key tasks execute in FIFO order; different keys run in parallel.
/// Idle keys are cleaned up automatically after the last task settles.
struct KeyedAsyncQueue {
    tails: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
}

impl KeyedAsyncQueue {
    fn new() -> Self {
        Self {
            tails: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Acquire a per-key permit. The returned guard must be held for the
    /// duration of the async work. Dropping it allows the next queued task
    /// for the same key to proceed.
    ///
    /// Performs lazy cleanup of idle semaphores to prevent unbounded growth
    /// in long-running deployments.
    async fn acquire(&self, key: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = {
            let mut tails = self.tails.lock().await;
            // Lazy cleanup: evict idle entries (available_permits == 1 means no one is holding or waiting)
            if tails.len() > 100 {
                tails.retain(|_, sem| Arc::strong_count(sem) > 1 || sem.available_permits() < 1);
            }
            tails
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
                .clone()
        };
        match sem.acquire_owned().await {
            Ok(permit) => Some(permit),
            Err(e) => {
                warn!(key, error = %e, "semaphore closed, skipping message");
                None
            }
        }
    }
}

// --- Socket Mode event loop ---

/// Hard cap on consecutive bot messages in a thread. Prevents runaway loops.
const MAX_CONSECUTIVE_BOT_TURNS: usize = 10;

/// Run the Slack adapter using Socket Mode (persistent WebSocket, no public URL needed).
/// Reconnects automatically on disconnect.
#[allow(clippy::too_many_arguments)]
pub async fn run_slack_adapter(
    bot_token: String,
    app_token: String,
    allow_all_channels: bool,
    allow_all_users: bool,
    allowed_channels: HashSet<String>,
    allowed_users: HashSet<String>,
    allow_bot_messages: AllowBots,
    trusted_bot_ids: HashSet<String>,
    allow_user_messages: AllowUsers,
    session_ttl: std::time::Duration,
    stt_config: SttConfig,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let adapter = Arc::new(SlackAdapter::new(bot_token.clone(), session_ttl, allow_bot_messages));
    let queue = Arc::new(KeyedAsyncQueue::new());

    loop {
        // Check for shutdown before (re)connecting
        if *shutdown_rx.borrow() {
            info!("Slack adapter shutting down");
            return Ok(());
        }

        let ws_url = match get_socket_mode_url(&app_token).await {
            Ok(url) => url,
            Err(e) => {
                error!("failed to get Socket Mode URL: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        info!(url = %ws_url, "connecting to Slack Socket Mode");

        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                info!("Slack Socket Mode connected");
                let (mut write, mut read) = ws_stream.split();

                loop {
                    tokio::select! {
                        msg_result = read.next() => {
                            let Some(msg_result) = msg_result else { break };
                            match msg_result {
                                Ok(tungstenite::Message::Text(text)) => {
                                    let envelope: serde_json::Value =
                                        match serde_json::from_str(&text) {
                                            Ok(v) => v,
                                            Err(_) => continue,
                                        };

                                    // Acknowledge the envelope immediately
                                    if let Some(envelope_id) = envelope["envelope_id"].as_str() {
                                        let ack = serde_json::json!({"envelope_id": envelope_id});
                                        let _ = write
                                            .send(tungstenite::Message::Text(ack.to_string()))
                                            .await;
                                    }

                                    // Route events
                                    if envelope["type"].as_str() == Some("events_api") {
                                        let event = &envelope["payload"]["event"];
                                        let event_type = event["type"].as_str().unwrap_or("");
                                        match event_type {
                                            "app_mention" => {
                                                // Apply bot gating for app_mention events (same rules as message events)
                                                let is_bot = event["bot_id"].is_string()
                                                    || event["subtype"].as_str() == Some("bot_message");
                                                if is_bot {
                                                    match allow_bot_messages {
                                                        AllowBots::Off => { continue; }
                                                        AllowBots::Mentions | AllowBots::All => {
                                                            if !trusted_bot_ids.is_empty() {
                                                                let event_bot_id = event["bot_id"].as_str().unwrap_or("");
                                                                let resolved = adapter.resolve_bot_user_id(event_bot_id).await;
                                                                let is_trusted = resolved.as_ref()
                                                                    .is_some_and(|uid| trusted_bot_ids.contains(uid.as_str()));
                                                                if !is_trusted {
                                                                    debug!(event_bot_id, resolved = ?resolved, "bot not in trusted_bot_ids, ignoring app_mention");
                                                                    continue;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                let event = event.clone();
                                                let adapter = adapter.clone();
                                                let bot_token = bot_token.clone();
                                                let allowed_channels = allowed_channels.clone();
                                                let allowed_users = allowed_users.clone();
                                                let stt_config = stt_config.clone();
                                                let router = router.clone();
                                                let queue = queue.clone();
                                                // Queue key: thread_ts if already in a thread, otherwise ts.
                                                // app_mention always has a channel context, so ts alone
                                                // is unique enough (unlike message events in DMs where
                                                // we prefix with channel_id to avoid ts collisions).
                                                let queue_key = event["thread_ts"]
                                                    .as_str()
                                                    .or_else(|| event["ts"].as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                tokio::spawn(async move {
                                                    let Some(_permit) = queue.acquire(&queue_key).await else { return };
                                                    handle_message(
                                                        &event,
                                                        true,
                                                        &adapter,
                                                        &bot_token,
                                                        allow_all_channels,
                                                        allow_all_users,
                                                        &allowed_channels,
                                                        &allowed_users,
                                                        &stt_config,
                                                        &router,
                                                    )
                                                    .await;
                                                });
                                            }
                                            "message" => {
                                                let channel_id = event["channel"].as_str().unwrap_or("");
                                                let has_thread = event["thread_ts"].is_string();
                                                let is_bot = event["bot_id"].is_string()
                                                    || event["subtype"].as_str() == Some("bot_message");
                                                let subtype = event["subtype"].as_str().unwrap_or("");
                                                let msg_text = event["text"].as_str().unwrap_or("");
                                                let mentions_bot = if let Some(bot_uid) = adapter.get_bot_user_id().await {
                                                    msg_text.contains(&format!("<@{bot_uid}>"))
                                                } else {
                                                    false
                                                };
                                                let is_dm = channel_id.starts_with('D');

                                                debug!(
                                                    channel_id,
                                                    has_thread,
                                                    is_bot,
                                                    is_dm,
                                                    subtype,
                                                    mentions_bot,
                                                    text = msg_text,
                                                    "message event received"
                                                );

                                                // Skip non-message subtypes
                                                let skip_subtype = matches!(subtype,
                                                    "message_changed" | "message_deleted" |
                                                    "channel_join" | "channel_leave" |
                                                    "channel_topic" | "channel_purpose"
                                                );
                                                if skip_subtype { continue; }

                                                // Skip messages that @mention the bot — app_mention handles those
                                                // (except in DMs where app_mention doesn't fire)
                                                if mentions_bot && !is_dm { continue; }

                                                // --- Bot message gating ---
                                                if is_bot {
                                                    let event_bot_id = event["bot_id"].as_str().unwrap_or("");
                                                    match allow_bot_messages {
                                                        AllowBots::Off => { continue; }
                                                        AllowBots::Mentions => {
                                                            if !mentions_bot { continue; }
                                                        }
                                                        AllowBots::All => {
                                                            // Loop protection: count consecutive bot msgs (fail-closed)
                                                            if let Some(thread_ts) = event["thread_ts"].as_str() {
                                                                let limit_str = (MAX_CONSECUTIVE_BOT_TURNS + 1).to_string();
                                                                match adapter.api_get(
                                                                    "conversations.replies",
                                                                    &[
                                                                        ("channel", channel_id),
                                                                        ("ts", thread_ts),
                                                                        ("limit", &limit_str),
                                                                        ("inclusive", "true"),
                                                                    ],
                                                                ).await {
                                                                    Ok(resp) => {
                                                                        if let Some(msgs) = resp["messages"].as_array() {
                                                                            let consecutive = msgs.iter().rev()
                                                                                .take_while(|m| {
                                                                                    m["bot_id"].is_string()
                                                                                        || m["subtype"].as_str() == Some("bot_message")
                                                                                })
                                                                                .count();
                                                                            if consecutive >= MAX_CONSECUTIVE_BOT_TURNS {
                                                                                warn!("bot turn cap reached ({MAX_CONSECUTIVE_BOT_TURNS}), ignoring");
                                                                                continue;
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        warn!(channel_id, thread_ts, error = %e, "failed to fetch thread for bot loop check, rejecting (fail-closed)");
                                                                        continue;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    // Check trusted_bot_ids
                                                    if !trusted_bot_ids.is_empty() {
                                                        let resolved = adapter.resolve_bot_user_id(event_bot_id).await;
                                                        let is_trusted = resolved
                                                            .as_ref()
                                                            .is_some_and(|uid| trusted_bot_ids.contains(uid.as_str()));
                                                        if !is_trusted {
                                                            debug!(event_bot_id, resolved = ?resolved, "bot not in trusted_bot_ids, ignoring");
                                                            continue;
                                                        }
                                                    }
                                                    // Bot messages must be in a thread (no top-level bot processing)
                                                    if !has_thread { continue; }
                                                }

                                                // --- User message gating ---
                                                if !is_bot {
                                                    if is_dm {
                                                        // DM: implicit mention — always process
                                                    } else {
                                                        match allow_user_messages {
                                                            AllowUsers::Mentions => {
                                                                if !mentions_bot { continue; }
                                                            }
                                                            AllowUsers::Involved | AllowUsers::MultibotMentions => {
                                                                if !has_thread {
                                                                    // Non-thread channel message: require mention
                                                                    // (app_mention handles this, but DMs don't get app_mention)
                                                                    continue;
                                                                }
                                                                // Thread message: check bot participation
                                                                let thread_ts = event["thread_ts"].as_str().unwrap_or("");
                                                                if !adapter.bot_participated_in_thread(channel_id, thread_ts).await {
                                                                    debug!(channel_id, thread_ts, "bot not involved in thread, ignoring");
                                                                    continue;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }

                                                // Dispatch to handle_message (serialized per thread)
                                                let event = event.clone();
                                                let adapter = adapter.clone();
                                                let bot_token = bot_token.clone();
                                                let allowed_channels = allowed_channels.clone();
                                                let allowed_users = allowed_users.clone();
                                                let stt_config = stt_config.clone();
                                                let router = router.clone();
                                                let queue = queue.clone();
                                                // Queue key: thread_ts if in a thread, otherwise channel:ts.
                                                // Prefixed with channel_id for non-thread messages because
                                                // DMs and channels can have overlapping ts values — the
                                                // prefix ensures keys are globally unique.
                                                let queue_key = event["thread_ts"]
                                                    .as_str()
                                                    .map(|s| s.to_string())
                                                    .unwrap_or_else(|| {
                                                        format!("{}:{}", channel_id, event["ts"].as_str().unwrap_or(""))
                                                    });
                                                tokio::spawn(async move {
                                                    let Some(_permit) = queue.acquire(&queue_key).await else { return };
                                                    handle_message(
                                                        &event,
                                                        is_dm,
                                                        &adapter,
                                                        &bot_token,
                                                        allow_all_channels,
                                                        allow_all_users,
                                                        &allowed_channels,
                                                        &allowed_users,
                                                        &stt_config,
                                                        &router,
                                                    )
                                                    .await;
                                                });
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                Ok(tungstenite::Message::Ping(data)) => {
                                    let _ = write.send(tungstenite::Message::Pong(data)).await;
                                }
                                Ok(tungstenite::Message::Close(_)) => {
                                    warn!("Slack Socket Mode connection closed by server");
                                    break;
                                }
                                Err(e) => {
                                    error!("Socket Mode read error: {e}");
                                    break;
                                }
                                _ => {}
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            info!("Slack adapter received shutdown signal");
                            let _ = write.send(tungstenite::Message::Close(None)).await;
                            return Ok(());
                        }
                    }
                }
            }
            Err(e) => {
                error!("failed to connect to Slack Socket Mode: {e}");
            }
        }

        warn!("reconnecting to Slack Socket Mode in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Call apps.connections.open to get a WebSocket URL for Socket Mode.
async fn get_socket_mode_url(app_token: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{SLACK_API}/apps.connections.open"))
        .header("Authorization", format!("Bearer {app_token}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    if json["ok"].as_bool() != Some(true) {
        let err = json["error"].as_str().unwrap_or("unknown");
        return Err(anyhow!("apps.connections.open: {err}"));
    }
    json["url"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("no url in apps.connections.open response"))
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    event: &serde_json::Value,
    strip_mentions: bool,
    adapter: &Arc<SlackAdapter>,
    bot_token: &str,
    allow_all_channels: bool,
    allow_all_users: bool,
    allowed_channels: &HashSet<String>,
    allowed_users: &HashSet<String>,
    stt_config: &SttConfig,
    router: &Arc<AdapterRouter>,
) {
    let channel_id = match event["channel"].as_str() {
        Some(ch) => ch.to_string(),
        None => return,
    };
    // Bot messages may lack "user" field — fall back to "bot_id" as sender identifier
    let user_id = match event["user"].as_str().or_else(|| event["bot_id"].as_str()) {
        Some(u) => u.to_string(),
        None => return,
    };
    let is_bot_msg = event["bot_id"].is_string()
        || event["subtype"].as_str() == Some("bot_message");
    let text = match event["text"].as_str() {
        Some(t) => t.to_string(),
        None => return,
    };
    let ts = match event["ts"].as_str() {
        Some(ts) => ts.to_string(),
        None => return,
    };
    let thread_ts = event["thread_ts"].as_str().map(|s| s.to_string());

    // Check allowed channels
    if !allow_all_channels && !allowed_channels.contains(&channel_id) {
        return;
    }

    // Check allowed users — skip for bot messages (they go through trusted_bot_ids instead)
    if !is_bot_msg && !allow_all_users && !allowed_users.contains(&user_id) {
        tracing::info!(user_id, "denied Slack user, ignoring");
        let msg_ref = MessageRef {
            channel: ChannelRef {
                platform: "slack".into(),
                channel_id: channel_id.clone(),
                thread_id: thread_ts.clone(),
                parent_id: None,
            },
            message_id: ts.clone(),
        };
        let _ = adapter.add_reaction(&msg_ref, "🚫").await;
        return;
    }

    // Strip bot mention from text for @mention events; DMs and thread follow-ups pass through as-is
    let prompt = if strip_mentions {
        strip_slack_mention(&text)
    } else {
        text.trim().to_string()
    };

    // Process file attachments (images, audio)
    let files = event["files"].as_array();
    let has_files = files.is_some_and(|f| !f.is_empty());

    if prompt.is_empty() && !has_files {
        return;
    }

    let mut extra_blocks = Vec::new();
    if let Some(files) = files {
        for file in files {
            let mimetype = file["mimetype"].as_str().unwrap_or("");
            let filename = file["name"].as_str().unwrap_or("file");
            let size = file["size"].as_u64().unwrap_or(0);
            // Slack private files require Bearer token to download
            let url = file["url_private_download"]
                .as_str()
                .or_else(|| file["url_private"].as_str())
                .unwrap_or("");

            if url.is_empty() {
                continue;
            }

            if media::is_audio_mime(mimetype) {
                if stt_config.enabled {
                    if let Some(transcript) = media::download_and_transcribe(
                        url,
                        filename,
                        mimetype,
                        size,
                        stt_config,
                        Some(bot_token),
                    ).await {
                        debug!(filename, chars = transcript.len(), "voice transcript injected");
                        extra_blocks.insert(0, ContentBlock::Text {
                            text: format!("[Voice message transcript]: {transcript}"),
                        });
                    }
                } else {
                    debug!(filename, "skipping audio attachment (STT disabled)");
                    let msg_ref = MessageRef {
                        channel: ChannelRef {
                            platform: "slack".into(),
                            channel_id: channel_id.clone(),
                            thread_id: thread_ts.clone(),
                            parent_id: None,
                        },
                        message_id: ts.clone(),
                    };
                    let _ = adapter.add_reaction(&msg_ref, "🎤").await;
                }
            } else if let Some(block) = media::download_and_encode_image(
                url,
                Some(mimetype),
                filename,
                size,
                Some(bot_token),
            ).await {
                debug!(filename, "adding image attachment");
                extra_blocks.push(block);
            }
        }
    }

    // Resolve Slack display name (best-effort, fallback to user_id)
    let display_name = adapter
        .resolve_user_name(&user_id)
        .await
        .unwrap_or_else(|| user_id.clone());

    let sender = SenderContext {
        schema: "openab.sender.v1".into(),
        sender_id: user_id.clone(),
        sender_name: display_name.clone(),
        display_name,
        channel: "slack".into(),
        channel_id: channel_id.clone(),
        thread_id: thread_ts.clone(),
        is_bot: is_bot_msg,
    };

    let trigger_msg = MessageRef {
        channel: ChannelRef {
            platform: "slack".into(),
            channel_id: channel_id.clone(),
            thread_id: thread_ts.clone(),
            parent_id: None,
        },
        message_id: ts.clone(),
    };

    // Determine thread: if already in a thread, continue it; otherwise start a new thread
    let thread_channel = ChannelRef {
        platform: "slack".into(),
        channel_id: channel_id.clone(),
        thread_id: Some(thread_ts.unwrap_or(ts)),
        parent_id: None,
    };

    // Serialize sender context with Slack-native key names so agents calling
    // the Slack API directly see "thread_ts" rather than the generic "thread_id".
    let sender_json = {
        let mut v = serde_json::to_value(&sender).unwrap();
        if let Some(obj) = v.as_object_mut() {
            if let Some(tid) = obj.remove("thread_id") {
                obj.insert("thread_ts".to_string(), tid);
            }
        }
        v.to_string()
    };

    let adapter_dyn: Arc<dyn ChatAdapter> = adapter.clone();
    if let Err(e) = router
        .handle_message(&adapter_dyn, &thread_channel, &sender_json, &prompt, extra_blocks, &trigger_msg)
        .await
    {
        error!("Slack handle_message error: {e}");
    }
}

static SLACK_MENTION_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<@[A-Z0-9]+>").unwrap());

fn strip_slack_mention(text: &str) -> String {
    SLACK_MENTION_RE.replace_all(text, "").trim().to_string()
}

/// Convert Markdown (as output by Claude Code) to Slack mrkdwn format.
fn markdown_to_mrkdwn(text: &str) -> String {
    static BOLD_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\*\*(.+?)\*\*").unwrap());
    static ITALIC_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\*([^*]+?)\*").unwrap());
    static LINK_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());
    static HEADING_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?m)^#{1,6}\s+(.+)$").unwrap());
    static CODE_BLOCK_LANG_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"```\w+\n").unwrap());

    // Order: bold first (** → placeholder), then italic (* → _), then restore bold
    let text = BOLD_RE.replace_all(text, "\x01$1\x02");       // **bold** → \x01bold\x02
    let text = ITALIC_RE.replace_all(&text, "_${1}_");         // *italic* → _italic_
    // Restore bold: \x01bold\x02 → *bold*
    let text = text.replace(['\x01', '\x02'], "*");
    let text = LINK_RE.replace_all(&text, "<$2|$1>");          // [text](url) → <url|text>
    let text = HEADING_RE.replace_all(&text, "*$1*");          // # heading → *heading*
    let text = CODE_BLOCK_LANG_RE.replace_all(&text, "```\n"); // ```rust → ```
    text.into_owned()
}

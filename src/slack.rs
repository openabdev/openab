use crate::acp::ContentBlock;
use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::bot_turns::{BotTurnTracker, TurnAction, TurnSeverity};
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
    /// Positive-only cache: thread_ts → cached_at for threads where other bots have posted.
    /// Like participation, a thread becoming multi-bot is irreversible (bot messages don't disappear).
    multibot_threads: tokio::sync::Mutex<HashMap<String, tokio::time::Instant>>,
    /// TTL for participation cache entries (matches session_ttl_hours from config).
    session_ttl: std::time::Duration,
}

impl SlackAdapter {
    pub fn new(bot_token: String, session_ttl: std::time::Duration, _allow_bot_messages: AllowBots) -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token,
            bot_user_id: tokio::sync::OnceCell::new(),
            user_cache: tokio::sync::Mutex::new(HashMap::new()),
            bot_id_cache: tokio::sync::Mutex::new(HashMap::new()),
            participated_threads: tokio::sync::Mutex::new(HashMap::new()),
            multibot_threads: tokio::sync::Mutex::new(HashMap::new()),
            session_ttl,
        }
    }

    /// Eagerly record that another bot has posted in a thread. Called from the
    /// event loop when a bot message arrives, so multibot detection doesn't
    /// depend on fetching thread history. Idempotent.
    async fn note_other_bot_in_thread(&self, thread_ts: &str) {
        let mut cache = self.multibot_threads.lock().await;
        cache.entry(thread_ts.to_string()).or_insert_with(tokio::time::Instant::now);
        enforce_cache_bounds(&mut cache, self.session_ttl);
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

    /// Check whether the bot has participated in a Slack thread and whether
    /// other bots have also posted in it.
    /// Returns `(involved, other_bot_present)`.
    /// Involved = parent message @mentions the bot OR any message in thread is from the bot.
    /// Fail-closed: returns `(false, false)` on API error (consistent with Discord's approach).
    /// Caches positive results only — both states are irreversible.
    async fn bot_participated_in_thread(&self, channel: &str, thread_ts: &str) -> (bool, bool) {
        let cached_involved = {
            let cache = self.participated_threads.lock().await;
            cache.get(thread_ts).is_some_and(|ts| ts.elapsed() < self.session_ttl)
        };
        let cached_multibot = {
            let cache = self.multibot_threads.lock().await;
            cache.get(thread_ts).is_some_and(|ts| ts.elapsed() < self.session_ttl)
        };

        // Eager multibot detection from message events populates the cache
        // before this runs. When already involved and cached, skip the fetch.
        if cached_involved {
            return (true, cached_multibot);
        }

        let bot_id = match self.get_bot_user_id().await {
            Some(id) => id,
            None => {
                warn!("cannot resolve bot user ID, rejecting (fail-closed)");
                return (false, false);
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
                return (false, false);
            }
        };
        let Some(messages) = json["messages"].as_array() else { return (false, false) };

        let parent_mentions_bot = messages
            .first()
            .and_then(|m| m["text"].as_str())
            .is_some_and(|text| text.contains(&format!("<@{bot_id}>")));

        let bot_posted = messages.iter().any(|m| m["user"].as_str() == Some(bot_id));

        let involved = parent_mentions_bot || bot_posted;
        let other_bot_present = cached_multibot
            || messages.iter().any(|m| {
                let is_bot_msg = m["bot_id"].is_string()
                    || m["subtype"].as_str() == Some("bot_message");
                is_bot_msg && m["user"].as_str() != Some(bot_id)
            });

        if involved {
            self.cache_participation(thread_ts).await;
        }
        if other_bot_present && !cached_multibot {
            self.note_other_bot_in_thread(thread_ts).await;
        }

        (involved, other_bot_present)
    }

    /// Insert a positive participation entry, enforcing cache bounds.
    async fn cache_participation(&self, thread_ts: &str) {
        let mut cache = self.participated_threads.lock().await;
        cache.insert(thread_ts.to_string(), tokio::time::Instant::now());
        enforce_cache_bounds(&mut cache, self.session_ttl);
    }
}

/// Shared eviction policy for positive-only caches.
/// First drops expired entries; if still over, drops the oldest half.
fn enforce_cache_bounds(
    cache: &mut HashMap<String, tokio::time::Instant>,
    ttl: std::time::Duration,
) {
    if cache.len() <= PARTICIPATION_CACHE_MAX {
        return;
    }
    cache.retain(|_, ts| ts.elapsed() < ttl);
    if cache.len() > PARTICIPATION_CACHE_MAX {
        let mut entries: Vec<_> = cache.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entries.sort_by_key(|(_, ts)| *ts);
        let evict_count = entries.len() / 2;
        for (key, _) in entries.into_iter().take(evict_count) {
            cache.remove(&key);
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

    fn use_streaming(&self, other_bot_present: bool) -> bool {
        !other_bot_present
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
    max_bot_turns: u32,
    session_ttl: std::time::Duration,
    stt_config: SttConfig,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let adapter = Arc::new(SlackAdapter::new(bot_token.clone(), session_ttl, allow_bot_messages));
    let queue = Arc::new(KeyedAsyncQueue::new());
    let bot_turns = Arc::new(tokio::sync::Mutex::new(BotTurnTracker::new(max_bot_turns)));

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

                                    // Slash commands and interactive block_actions aren't
                                    // handled on Slack: slash commands are blocked by Slack
                                    // in thread composers, and the channel-level delivery
                                    // lacks the thread_ts needed to route to a session.
                                    // Ack only; ignore payload.
                                    match envelope["type"].as_str() {
                                        Some("slash_commands") | Some("interactive") => {
                                            debug!(
                                                envelope_type = envelope["type"].as_str().unwrap_or(""),
                                                "ignoring Slack envelope type (not supported on this adapter)"
                                            );
                                            continue;
                                        }
                                        _ => {}
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
                                                let bot_uid_opt = adapter.get_bot_user_id().await.map(|s| s.to_string());
                                                let mentions_bot = bot_uid_opt
                                                    .as_ref()
                                                    .is_some_and(|bot_uid| msg_text.contains(&format!("<@{bot_uid}>")));
                                                let is_dm = channel_id.starts_with('D');
                                                let event_user_id = event["user"].as_str();
                                                let is_own_bot_msg = is_bot
                                                    && bot_uid_opt.as_deref().is_some()
                                                    && event_user_id == bot_uid_opt.as_deref();

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

                                                // --- Eager multibot detection ---
                                                // Runs before self-check and bot gating so we always detect
                                                // other bots even when allow_bot_messages=Off filters them out.
                                                // Matches Discord #481 ordering.
                                                if is_bot && !is_own_bot_msg {
                                                    if let Some(thread_ts) = event["thread_ts"].as_str() {
                                                        adapter.note_other_bot_in_thread(thread_ts).await;
                                                    }
                                                }

                                                // --- Bot turn tracking ---
                                                // Runs before self-check so ALL bot messages (including own)
                                                // count toward the per-thread limit. Matches Discord #483.
                                                // Keyed on thread_ts when in a thread, else channel:ts (the
                                                // same key shape used for per-thread queueing below).
                                                // Non-thread messages get a unique key per message, so the
                                                // counter never accumulates — intentional, because bot-to-bot
                                                // loops only happen inside threads.
                                                let turn_key = if let Some(thread_ts) = event["thread_ts"].as_str() {
                                                    thread_ts.to_string()
                                                } else {
                                                    format!("{}:{}", channel_id, event["ts"].as_str().unwrap_or(""))
                                                };
                                                {
                                                    let mut tracker = bot_turns.lock().await;
                                                    if is_bot {
                                                        match tracker.classify_bot_message(&turn_key) {
                                                            TurnAction::Continue => {}
                                                            TurnAction::SilentStop => continue,
                                                            TurnAction::WarnAndStop { severity, turns, user_message } => {
                                                                match severity {
                                                                    TurnSeverity::Hard => warn!(channel_id, turns, "hard bot turn limit reached"),
                                                                    TurnSeverity::Soft => info!(channel_id, turns, max = max_bot_turns, "soft bot turn limit reached"),
                                                                }
                                                                if !is_own_bot_msg {
                                                                    let warn_channel = ChannelRef {
                                                                        platform: "slack".into(),
                                                                        channel_id: channel_id.to_string(),
                                                                        thread_id: event["thread_ts"].as_str().map(|s| s.to_string()),
                                                                        parent_id: None,
                                                                    };
                                                                    let _ = adapter.send_message(&warn_channel, &user_message).await;
                                                                }
                                                                continue;
                                                            }
                                                        }
                                                    } else if is_plain_user_message(subtype, msg_text) {
                                                        tracker.on_human_message(&turn_key);
                                                    }
                                                }

                                                // Ignore own bot messages (after counting toward turns)
                                                if is_own_bot_msg { continue; }

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
                                                            AllowUsers::Involved => {
                                                                if !has_thread {
                                                                    continue;
                                                                }
                                                                let thread_ts = event["thread_ts"].as_str().unwrap_or("");
                                                                let (involved, _) = adapter
                                                                    .bot_participated_in_thread(channel_id, thread_ts)
                                                                    .await;
                                                                if !involved {
                                                                    debug!(channel_id, thread_ts, "bot not involved in thread, ignoring");
                                                                    continue;
                                                                }
                                                            }
                                                            AllowUsers::MultibotMentions => {
                                                                if !has_thread {
                                                                    continue;
                                                                }
                                                                let thread_ts = event["thread_ts"].as_str().unwrap_or("");
                                                                let (involved, other_bot) = adapter
                                                                    .bot_participated_in_thread(channel_id, thread_ts)
                                                                    .await;
                                                                if !involved {
                                                                    debug!(channel_id, thread_ts, "bot not involved in thread, ignoring");
                                                                    continue;
                                                                }
                                                                // In multi-bot threads, require @mention — mirrors
                                                                // Discord's `should_process_user_message`. In practice
                                                                // mention-bearing message events are already deduped
                                                                // earlier (app_mention handles the @-path), so this
                                                                // branch rarely sees `mentions_bot == true`, but keep
                                                                // the explicit check so the logic is self-consistent
                                                                // and survives changes to the earlier dedup.
                                                                if other_bot && !mentions_bot {
                                                                    debug!(channel_id, thread_ts, "multi-bot thread without @mention, ignoring");
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

    // Resolve mentions: strip only this bot's own trigger mention so the LLM
    // can still @-mention other users in its reply.
    let bot_id = adapter.get_bot_user_id().await;
    let prompt = resolve_slack_mentions(&text, bot_id);

    // Process file attachments (images, audio)
    let files = event["files"].as_array();
    let has_files = files.is_some_and(|f| !f.is_empty());

    if prompt.is_empty() && !has_files {
        return;
    }

    // Caps mirror Discord's text-file attachment flow (PR #291) so both
    // adapters apply the same limits: 5 files or 1 MB of text per message.
    const TEXT_TOTAL_CAP: u64 = 1024 * 1024;
    const TEXT_FILE_COUNT_CAP: u32 = 5;

    let mut extra_blocks = Vec::new();
    let mut text_file_bytes: u64 = 0;
    let mut text_file_count: u32 = 0;

    if let Some(files) = files {
        for file in files {
            let mimetype_raw = file["mimetype"].as_str().unwrap_or("");
            let mimetype = strip_mime_params(mimetype_raw);
            let filename = file["name"].as_str().unwrap_or("file");
            let size = file["size"].as_u64().unwrap_or(0);
            // Slack private files require Bearer token to download
            let url = slack_file_download_url(file);

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
            } else if media::is_text_file(filename, Some(mimetype)) {
                if text_file_count >= TEXT_FILE_COUNT_CAP {
                    debug!(filename, count = text_file_count, "text file count cap reached, skipping");
                    continue;
                }
                // Pre-check with Slack-reported size as a fast path when the
                // field is populated. Slack can report `size == 0` for
                // externally-backed files, so this is advisory only — the
                // authoritative cap check happens after download using
                // `actual_bytes`.
                if size > 0 && text_file_bytes + size > TEXT_TOTAL_CAP {
                    debug!(filename, total = text_file_bytes, "text attachments total exceeds 1MB cap, skipping remaining");
                    continue;
                }
                if let Some((block, actual_bytes)) = media::download_and_read_text_file(
                    url,
                    filename,
                    size,
                    Some(bot_token),
                ).await {
                    if text_file_bytes + actual_bytes > TEXT_TOTAL_CAP {
                        debug!(
                            filename,
                            running = text_file_bytes,
                            actual = actual_bytes,
                            "text attachments total exceeds 1MB cap after download, dropping file",
                        );
                        continue;
                    }
                    text_file_bytes += actual_bytes;
                    text_file_count += 1;
                    debug!(filename, "adding text file attachment");
                    extra_blocks.push(block);
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
    let other_bot_present = {
        let cache = adapter.multibot_threads.lock().await;
        cache.contains_key(&thread_channel.channel_id)
    };
    if let Err(e) = router
        .handle_message(
            &adapter_dyn,
            &thread_channel,
            &sender_json,
            &prompt,
            extra_blocks,
            &trigger_msg,
            other_bot_present,
        )
        .await
    {
        error!("Slack handle_message error: {e}");
    }
}

/// Strip only the bot's own `<@BOT_UID>` trigger mention.
/// Other users' mentions stay intact so the LLM can @-mention them back.
/// If the bot UID isn't known, fall back to returning the text trimmed —
/// safer than stripping all mentions and losing user addressability.
fn resolve_slack_mentions(text: &str, bot_id: Option<&str>) -> String {
    match bot_id {
        Some(id) => text.replace(&format!("<@{id}>"), "").trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Pick the best download URL for a Slack file object. `url_private_download`
/// streams the raw bytes; `url_private` is the fallback for older file shapes.
/// Returns `""` when neither is present (caller should skip the file).
fn slack_file_download_url(file: &serde_json::Value) -> &str {
    file["url_private_download"]
        .as_str()
        .or_else(|| file["url_private"].as_str())
        .unwrap_or("")
}

/// Strip MIME parameters like `; charset=utf-8` so type-detection helpers see
/// the bare media type. Slack occasionally sends mimetypes like
/// `text/plain; charset=utf-8`; `media::is_text_file` expects the bare form.
fn strip_mime_params(mimetype: &str) -> &str {
    mimetype.split(';').next().unwrap_or(mimetype).trim()
}

/// True only when a Slack non-bot event represents a real user message
/// that should reset the bot-turn counter.
///
/// Many Slack subtypes (pinned_item, channel_name, channel_archive,
/// group_join / group_leave / group_topic / group_purpose, reminder_add,
/// tombstone, …) carry a `user` field so the event loop sees
/// `is_bot == false`, but they represent administrative/system actions,
/// not conversation. Resetting the counter on them would let runaway
/// bot-to-bot loops re-arm whenever any pin / rename / archive happens.
///
/// Mirrors Discord's `MessageType::Regular | InlineReply` + non-empty
/// content gate in `src/discord.rs`. Regression parity for
/// openabdev/openab#497.
fn is_plain_user_message(subtype: &str, text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    matches!(
        subtype,
        "" | "me_message" | "thread_broadcast" | "file_share",
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::ChatAdapter;

    /// Bot's own `<@UID>` trigger mention is stripped.
    #[test]
    fn resolve_mentions_strips_bot_mention() {
        let out = resolve_slack_mentions("<@U1BOT> hello", Some("U1BOT"));
        assert_eq!(out, "hello");
    }

    /// Other users' mentions are preserved so the LLM can address them back —
    /// this is the core fix: the old `strip_slack_mention` wiped all `<@...>`.
    #[test]
    fn resolve_mentions_preserves_other_user_mentions() {
        let out = resolve_slack_mentions("<@U1BOT> say hi to <@U2ALICE>", Some("U1BOT"));
        assert_eq!(out, "say hi to <@U2ALICE>");
    }

    /// Multiple occurrences of the bot mention all get stripped.
    #[test]
    fn resolve_mentions_strips_repeated_bot_mentions() {
        let out = resolve_slack_mentions("<@U1BOT> ping <@U1BOT>", Some("U1BOT"));
        assert_eq!(out, "ping");
    }

    /// When the bot UID is unknown, fall back to preserving the text
    /// (safer than stripping all user mentions).
    #[test]
    fn resolve_mentions_unknown_bot_preserves_all() {
        let out = resolve_slack_mentions("<@U1BOT> hi <@U2ALICE>", None);
        assert_eq!(out, "<@U1BOT> hi <@U2ALICE>");
    }

    // --- is_plain_user_message tests (regression for openabdev/openab#497 parity) ---

    /// Empty message text never counts as a user message (regardless of subtype).
    #[test]
    fn empty_text_is_not_plain_user_message() {
        assert!(!is_plain_user_message("", ""));
        assert!(!is_plain_user_message("me_message", ""));
    }

    /// No subtype + non-empty text = plain user message (the common case).
    #[test]
    fn no_subtype_nonempty_text_is_plain_user_message() {
        assert!(is_plain_user_message("", "hello"));
    }

    /// Whitelisted subtypes with non-empty text are user messages.
    #[test]
    fn whitelisted_subtypes_are_plain_user_messages() {
        assert!(is_plain_user_message("me_message", "waves"));
        assert!(is_plain_user_message("thread_broadcast", "see channel"));
        assert!(is_plain_user_message("file_share", "caption"));
    }

    /// System-ish subtypes (even from real users) are NOT user messages —
    /// resetting the counter on them would let bot-to-bot loops re-arm.
    #[test]
    fn system_subtypes_are_not_plain_user_messages() {
        for subtype in [
            "pinned_item",
            "unpinned_item",
            "channel_name",
            "channel_archive",
            "channel_unarchive",
            "group_join",
            "group_leave",
            "group_topic",
            "group_purpose",
            "reminder_add",
            "tombstone",
        ] {
            assert!(
                !is_plain_user_message(subtype, "some text"),
                "subtype {subtype} must not count as a user message",
            );
        }
    }

    // --- slack_file_download_url tests ---

    /// Prefers url_private_download when both fields are present —
    /// that endpoint always streams raw bytes even for browser-previewed types.
    #[test]
    fn slack_file_url_prefers_download_variant() {
        let file = serde_json::json!({
            "url_private_download": "https://files.slack.com/.../download/log.txt",
            "url_private":          "https://files.slack.com/.../preview/log.txt",
        });
        assert_eq!(
            slack_file_download_url(&file),
            "https://files.slack.com/.../download/log.txt",
        );
    }

    /// Falls back to url_private when url_private_download is absent.
    #[test]
    fn slack_file_url_falls_back_to_private() {
        let file = serde_json::json!({
            "url_private": "https://files.slack.com/.../log.txt",
        });
        assert_eq!(
            slack_file_download_url(&file),
            "https://files.slack.com/.../log.txt",
        );
    }

    /// Externally-backed files with no private URL return empty — caller skips.
    #[test]
    fn slack_file_url_empty_for_external_only() {
        let file = serde_json::json!({
            "external_type": "gdrive",
            "permalink": "https://docs.google.com/...",
        });
        assert_eq!(slack_file_download_url(&file), "");
    }

    // --- strip_mime_params tests ---

    /// MIME with charset parameter strips to bare media type.
    #[test]
    fn strip_mime_params_removes_charset() {
        assert_eq!(strip_mime_params("text/plain; charset=utf-8"), "text/plain");
    }

    /// Bare MIME is unchanged.
    #[test]
    fn strip_mime_params_bare_unchanged() {
        assert_eq!(strip_mime_params("image/png"), "image/png");
    }

    /// Empty input is unchanged.
    #[test]
    fn strip_mime_params_empty() {
        assert_eq!(strip_mime_params(""), "");
    }

    /// Surrounding whitespace is trimmed.
    #[test]
    fn strip_mime_params_trims_whitespace() {
        assert_eq!(strip_mime_params("  text/plain  "), "text/plain");
    }

    /// Per-thread streaming: ON by default, OFF when another bot is present (#534).
    #[test]
    fn streaming_per_thread() {
        let ttl = std::time::Duration::from_secs(300);
        let adapter = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Mentions);

        assert!(adapter.use_streaming(false), "should stream when no other bot");
        assert!(!adapter.use_streaming(true), "should NOT stream when other bot present");
    }
}

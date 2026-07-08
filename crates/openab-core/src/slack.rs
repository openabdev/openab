use crate::acp::ContentBlock;
use crate::adapter::{ChannelRef, ChatAdapter, MessageRef, SenderContext};
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

/// Maximum entries in the streams map before eviction (safety net for
/// aborted turns that begin a stream but never reach stream_finish).
const STREAM_CACHE_MAX: usize = 1024;

#[derive(Default)]
struct StreamEntry {
    active: bool,
    degraded_buf: String,
}

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
    /// Persistent disk cache for multibot thread detection (survives restarts).
    multibot_cache: crate::multibot_cache::MultibotCache,
    /// TTL for participation cache entries (matches session_ttl_hours from config).
    session_ttl: std::time::Duration,
    /// Assistant mode: stream via chat.startStream + assistant.threads.setStatus.
    assistant_mode: bool,
    /// Master streaming switch. When false, the adapter always posts a single
    /// final message (send-once): no native streaming, no post+edit placeholder.
    /// Default true. Set false to avoid streamed-message edit states (e.g. a
    /// reply that @-mentions another bot re-firing app_mention in multi-agent
    /// threads). Mirrors `[gateway] streaming`.
    streaming: bool,
    /// streaming message ts → state. active=false = degraded (post+edit fallback).
    /// Lifecycle: stream_begin inserts, stream_finish removes; insert_stream
    /// bounds the map (STREAM_CACHE_MAX) as a safety net against aborted turns.
    streams: tokio::sync::Mutex<HashMap<String, StreamEntry>>,
}

impl SlackAdapter {
    pub fn new(
        bot_token: String,
        session_ttl: std::time::Duration,
        _allow_bot_messages: AllowBots,
        assistant_mode: bool,
        multibot_cache: crate::multibot_cache::MultibotCache,
        streaming: bool,
    ) -> Self {
        Self {
            // Bound every Slack Web API call; an unbounded inline gating call in the
            // read loop could otherwise stall the Socket Mode idle-timeout watchdog.
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            bot_token,
            bot_user_id: tokio::sync::OnceCell::new(),
            user_cache: tokio::sync::Mutex::new(HashMap::new()),
            bot_id_cache: tokio::sync::Mutex::new(HashMap::new()),
            participated_threads: tokio::sync::Mutex::new(HashMap::new()),
            multibot_threads: tokio::sync::Mutex::new(HashMap::new()),
            multibot_cache,
            session_ttl,
            assistant_mode,
            streaming,
            streams: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the bot token for use in API calls outside the adapter.
    pub fn bot_token(&self) -> &str {
        &self.bot_token
    }

    /// Eagerly record that another bot has posted in a thread. Called from the
    /// event loop when a bot message arrives, so multibot detection doesn't
    /// depend on fetching thread history. Idempotent.
    async fn note_other_bot_in_thread(&self, thread_ts: &str) {
        {
            let mut cache = self.multibot_threads.lock().await;
            cache
                .entry(thread_ts.to_string())
                .or_insert_with(tokio::time::Instant::now);
            enforce_cache_bounds(&mut cache, self.session_ttl);
        }
        // Persist to disk — multibot is irreversible
        self.multibot_cache.mark_multibot(thread_ts).await;
    }


    /// Insert a stream entry, bounding the map so aborted turns (begin without a
    /// matching finish) can't leak unboundedly. Normal lifecycle: stream_begin
    /// inserts, stream_finish removes.
    async fn insert_stream(&self, ts: String, entry: StreamEntry) {
        let mut map = self.streams.lock().await;
        if map.len() >= STREAM_CACHE_MAX {
            // Only evict inactive (degraded/stale) streams to avoid cutting off
            // active streams mid-turn. If no inactive entries exist, fall through
            // and allow the map to grow slightly beyond the soft cap.
            let evict: Vec<String> = map
                .iter()
                .filter(|(_, e)| !e.active)
                .map(|(k, _)| k.clone())
                .collect();
            for k in evict {
                map.remove(&k);
            }
        }
        map.insert(ts, entry);
    }

    /// Accumulate a delta into a degraded stream's buffer and return the new
    /// cumulative text. Returns None if no (degraded) stream entry exists for
    /// `ts` — never resurrects a removed/absent stream. No network I/O.
    async fn accumulate_degraded(&self, ts: &str, delta: &str) -> Option<String> {
        let mut map = self.streams.lock().await;
        let entry = map.get_mut(ts)?;
        entry.degraded_buf.push_str(delta);
        Some(entry.degraded_buf.clone())
    }

    /// Get the bot's own Slack user ID (cached after first call).
    async fn get_bot_user_id(&self) -> Option<&str> {
        self.bot_user_id
            .get_or_try_init(|| async {
                let resp = self
                    .api_post("auth.test", serde_json::json!({}))
                    .await
                    .map_err(|e| anyhow!("auth.test failed: {e}"))?;
                resp["user_id"]
                    .as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| anyhow!("no user_id in auth.test response"))
            })
            .await
            .inspect_err(|e| warn!(error = %e, "bot user ID unavailable; mention detection may suppress bot messages under Mentions mode"))
            .ok()
            .map(|s| s.as_str())
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
            .api_post("users.info", serde_json::json!({ "user": user_id }))
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
        let name = user.get("name").and_then(|v| v.as_str());
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
        if bot_id.is_empty() {
            return None;
        }

        {
            let cache = self.bot_id_cache.lock().await;
            if let Some(user_id) = cache.get(bot_id) {
                return Some(user_id.clone());
            }
        }

        let resp = self
            .api_post("bots.info", serde_json::json!({ "bot": bot_id }))
            .await
            .inspect_err(|e| {
                warn!(
                    bot_id,
                    error = %e,
                    "failed to resolve Slack bot ID via bots.info"
                )
            })
            .ok()?;
        let user_id = resp.get("bot")?.get("user_id")?.as_str()?.to_string();

        self.bot_id_cache
            .lock()
            .await
            .insert(bot_id.to_string(), user_id.clone());

        Some(user_id)
    }

    async fn trusted_bot_ids_contains(
        &self,
        trusted_bot_ids: &HashSet<String>,
        event_bot_id: &str,
    ) -> bool {
        if trusted_bot_ids.is_empty() {
            return true;
        }
        if bot_id_matches_trusted(trusted_bot_ids, event_bot_id, None) {
            return true;
        }
        let resolved = self.resolve_bot_user_id(event_bot_id).await;
        bot_id_matches_trusted(trusted_bot_ids, event_bot_id, resolved.as_deref())
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
            cache
                .get(thread_ts)
                .is_some_and(|ts| ts.elapsed() < self.session_ttl)
        };
        let cached_multibot = {
            let cache = self.multibot_threads.lock().await;
            cache
                .get(thread_ts)
                .is_some_and(|ts| ts.elapsed() < self.session_ttl)
        } || self.multibot_cache.is_multibot(thread_ts);

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
        let Some(messages) = json["messages"].as_array() else {
            return (false, false);
        };

        let parent_mentions_bot = messages
            .first()
            .and_then(|m| m["text"].as_str())
            .is_some_and(|text| text_mentions_uid(text, bot_id));

        let bot_posted = messages.iter().any(|m| m["user"].as_str() == Some(bot_id));

        let involved = parent_mentions_bot || bot_posted;
        // other_bot_present relies solely on early detection + disk cache;
        // no longer scanned from fetched messages (200-msg window was unreliable).
        let other_bot_present = cached_multibot;

        if involved {
            self.cache_participation(thread_ts).await;
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
        // Match the Block Kit `markdown` block cap (12k) minus headroom. Messages
        // are sent as markdown blocks, so the old 4000 mrkdwn-era limit would
        // split long replies (and Markdown tables) across messages needlessly —
        // a mid-table split renders as raw pipes. 11_900 keeps typical tables in
        // one block and cuts message-spam on long replies.
        MARKDOWN_BLOCK_LIMIT
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let thread_ts = channel.thread_id.as_deref();
        let body = build_post_message_body(&channel.channel_id, thread_ts, content);
        let resp = match self.api_post("chat.postMessage", body).await {
            Ok(r) => r,
            // Graceful degradation: if the `blocks` payload is rejected (workspace
            // lacks the markdown block, or content exceeds the cumulative block
            // cap), retry text-only so the message still lands (mrkdwn fallback)
            // instead of failing outright.
            Err(e) if is_block_payload_rejected(&e) => {
                warn!(error = %e, "markdown block rejected; retrying chat.postMessage text-only");
                let fallback = build_post_message_text_only(&channel.channel_id, thread_ts, content);
                self.api_post("chat.postMessage", fallback).await?
            }
            Err(e) => return Err(e),
        };
        let ts = resp["ts"]
            .as_str()
            .ok_or_else(|| anyhow!("no ts in chat.postMessage response"))?;
        Ok(MessageRef {
            channel: ChannelRef {
                platform: "slack".into(),
                channel_id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
                parent_id: None,
                origin_event_id: None,
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
            origin_event_id: None,
        })
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let name = unicode_to_slack_emoji(emoji);
        match self
            .api_post(
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
        match self
            .api_post(
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
        let body = build_update_body(&msg.channel.channel_id, &msg.message_id, content);
        match self.api_post("chat.update", body).await {
            Ok(_) => Ok(()),
            // See send_message: degrade to text-only if the blocks payload is rejected.
            Err(e) if is_block_payload_rejected(&e) => {
                warn!(error = %e, "markdown block rejected; retrying chat.update text-only");
                let fallback =
                    build_update_text_only(&msg.channel.channel_id, &msg.message_id, content);
                self.api_post("chat.update", fallback).await?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn use_streaming(&self, other_bot_present: bool) -> bool {
        self.streaming && !other_bot_present
    }

    fn renders_native_tables(&self, _platform: &str) -> bool {
        true
    }

    fn uses_assistant_status(&self) -> bool {
        self.assistant_mode
    }

    fn uses_native_streaming(&self, other_bot_present: bool) -> bool {
        let native = self.streaming && self.assistant_mode && !other_bot_present;
        debug!(
            streaming = self.streaming,
            assistant_mode = self.assistant_mode,
            other_bot_present,
            native,
            "slack assistant_mode decision (per turn)"
        );
        native
    }

    async fn stream_begin(
        &self,
        channel: &ChannelRef,
        recipient: Option<(String, String)>,
    ) -> Result<MessageRef> {
        let thread_ts = channel.thread_id.clone().unwrap_or_default();
        // recipient is bound to this turn (captured at message arrival, carried on
        // BufferedMessage) — no shared thread cache, so no cross-turn race.
        let make_ref = |ts: String| MessageRef {
            channel: ChannelRef {
                platform: "slack".into(),
                channel_id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
                parent_id: None,
                origin_event_id: None,
            },
            message_id: ts,
        };

        if let Some((user_id, team_id)) = recipient {
            let body = build_start_stream_body(&channel.channel_id, &thread_ts, &user_id, &team_id);
            match self.api_post("chat.startStream", body).await {
                Ok(resp) => {
                    if let Some(ts) = resp["ts"].as_str() {
                        self.insert_stream(
                            ts.to_string(),
                            StreamEntry { active: true, degraded_buf: String::new() },
                        )
                        .await;
                        return Ok(make_ref(ts.to_string()));
                    }
                    error!("chat.startStream ok but no ts; falling back to post+edit");
                }
                Err(e) => {
                    error!(error = %e, "chat.startStream failed; falling back to post+edit for this turn");
                }
            }
        } else {
            // Expected for bot-authored turns (no recipient bound) and non-user
            // triggers, so warn! rather than error! to avoid on-call noise.
            warn!(thread_ts, "no recipient for turn; falling back to post+edit");
        }

        // Degraded fallback: plain placeholder via send_message; mark inactive.
        let msg = self.send_message(channel, "…").await?;
        self.insert_stream(
            msg.message_id.clone(),
            StreamEntry { active: false, degraded_buf: String::new() },
        )
        .await;
        Ok(msg)
    }

    async fn stream_append(&self, msg: &MessageRef, delta: &str) -> Result<()> {
        let ts = &msg.message_id;
        let active = {
            let map = self.streams.lock().await;
            map.get(ts).map(|e| e.active).unwrap_or(false)
        };
        if active {
            let body = build_append_stream_body(&msg.channel.channel_id, ts, delta);
            if let Err(e) = self.api_post("chat.appendStream", body).await {
                warn!(error = %e, "chat.appendStream failed (cosmetic; final replace will correct)");
            }
        } else if let Some(cumulative) = self.accumulate_degraded(ts, delta).await {
            let _ = self.edit_message(msg, &cumulative).await; // cosmetic mid-stream
        }
        Ok(())
    }

    async fn stream_finish(&self, msg: &MessageRef, final_content: &str) -> Result<()> {
        let ts = &msg.message_id;
        let active = {
            let map = self.streams.lock().await;
            map.get(ts).map(|e| e.active).unwrap_or(false)
        };
        if active {
            // Close the native stream WITHOUT re-sending content. The reply was
            // already streamed live via chat.appendStream; stopStream's
            // `markdown_text` *appends* (it does not replace), so passing the full
            // content here duplicates the whole reply (#1055). Close only, then
            // replace with the finalized content via chat.update below.
            let close = serde_json::json!({ "channel": msg.channel.channel_id, "ts": ts });
            if let Err(e) = self.api_post("chat.stopStream", close).await {
                warn!(error = %e, "chat.stopStream(close) failed; continuing to final replace");
            }
        }
        // Replace with the finalized content (Block Kit markdown). For the active
        // path this overwrites the streamed preview with a single clean copy
        // (rich rendering + native tables); for the degraded path it is the final
        // post+edit update. chat.update replaces, so there is no duplication.
        if let Err(e) = self.edit_message(msg, final_content).await {
            if active {
                // The native stream already delivered the reply (chat.appendStream),
                // and stopStream left it in place. Do NOT postMessage a fallback
                // here — that would post a duplicate copy. Keep the streamed
                // content as the final message.
                warn!(error = %e, "final chat.update failed; keeping streamed content (no duplicate post)");
            } else {
                // Degraded path: no streamed content exists (post+edit placeholder),
                // so post the final as a new message to avoid losing the reply.
                warn!(error = %e, "final chat.update failed; trying postMessage");
                if let Err(e2) = self.send_message(&msg.channel, final_content).await {
                    error!(error = %e2, "final postMessage also failed; reply may be incomplete");
                }
            }
        }
        self.streams.lock().await.remove(ts);
        Ok(())
    }

    async fn set_status(&self, channel: &ChannelRef, status: &str) -> Result<()> {
        let thread_ts = channel.thread_id.clone().unwrap_or_default();
        let body = build_set_status_body(&channel.channel_id, &thread_ts, status);
        if let Err(e) = self.api_post("assistant.threads.setStatus", body).await {
            warn!(error = %e, status, "assistant.threads.setStatus failed (cosmetic)");
        }
        Ok(())
    }
}

// --- Socket Mode event loop ---

/// Hard cap on consecutive bot messages in a thread. Prevents runaway loops.
const MAX_CONSECUTIVE_BOT_TURNS: usize = 1000;

/// Socket Mode keepalive. Slack's inbound WebSocket can go half-open (e.g. a NAT
/// idle-timeout silently drops inbound frames with no Close/FIN), which leaves
/// `read.next()` blocked forever, so the reconnect loop never fires and the bot
/// goes deaf while still showing as connected. We proactively ping and force a
/// reconnect when no inbound frame (including Slack's own pings) has arrived
/// within the idle window. Reconnect backoff mirrors the gateway adapter.
const PING_INTERVAL_SECS: u64 = 30;
const IDLE_TIMEOUT_SECS: u64 = 75;
const MAX_BACKOFF_SECS: u64 = 30;

/// Next reconnect delay: double, capped. Reset to 1 on a successful connect.
fn next_backoff(cur: u64) -> u64 {
    (cur * 2).min(MAX_BACKOFF_SECS)
}

/// The socket is considered dead (half-open) when no inbound frame has arrived
/// within `timeout`; Slack sends periodic pings, so silence past the window
/// means the inbound path is gone.
fn socket_idle(since_last_inbound: std::time::Duration, timeout: std::time::Duration) -> bool {
    since_last_inbound >= timeout
}

/// Run the Slack adapter using Socket Mode (persistent WebSocket, no public URL needed).
/// Reconnects automatically on disconnect.
#[allow(clippy::too_many_arguments)]
pub async fn run_slack_adapter(
    adapter: Arc<SlackAdapter>,
    app_token: String,
    allow_all_channels: bool,
    allow_all_users: bool,
    allowed_channels: HashSet<String>,
    allowed_users: HashSet<String>,
    allow_bot_messages: AllowBots,
    trusted_bot_ids: HashSet<String>,
    allow_user_messages: AllowUsers,
    max_bot_turns: u32,
    stt_config: SttConfig,
    mut shutdown_rx: watch::Receiver<bool>,
    dispatcher: Arc<crate::dispatch::Dispatcher>,
) -> Result<()> {
    let bot_token = adapter.bot_token().to_string();
    let bot_turns = Arc::new(tokio::sync::Mutex::new(BotTurnTracker::new(max_bot_turns)));
    // Warm the bot-user-id cache once so the per-message path never does the
    // cold-cache `auth.test` inline in the read loop.
    let _ = adapter.get_bot_user_id().await;
    let mut backoff_secs = 1u64;

    loop {
        // Check for shutdown before (re)connecting
        if *shutdown_rx.borrow() {
            info!("Slack adapter shutting down");
            return Ok(());
        }

        let ws_url = match get_socket_mode_url(&app_token).await {
            Ok(url) => url,
            Err(e) => {
                error!(err = %e, backoff = backoff_secs, "failed to get Socket Mode URL, retrying");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = shutdown_rx.changed() => { return Ok(()); }
                }
                backoff_secs = next_backoff(backoff_secs);
                continue;
            }
        };
        info!(url = %ws_url, "connecting to Slack Socket Mode");

        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                info!("Slack Socket Mode connected");
                backoff_secs = 1; // reset on success
                let (mut write, mut read) = ws_stream.split();
                let mut ping_interval =
                    tokio::time::interval(std::time::Duration::from_secs(PING_INTERVAL_SECS));
                ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_inbound = std::time::Instant::now();

                loop {
                    tokio::select! {
                        msg_result = read.next() => {
                            last_inbound = std::time::Instant::now();
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
                                                                let is_trusted = adapter
                                                                    .trusted_bot_ids_contains(&trusted_bot_ids, event_bot_id)
                                                                    .await;
                                                                if !is_trusted {
                                                                    debug!(event_bot_id, "bot not in trusted_bot_ids, ignoring app_mention");
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
                                                let dispatcher = dispatcher.clone();
                                                let team_id = envelope["payload"]["team_id"]
                                                    .as_str()
                                                    .unwrap_or("")
                                                    .to_string();
                                                tokio::spawn(async move {
                                                    handle_message(
                                                        &event,
                                                        &team_id,
                                                        &adapter,
                                                        &bot_token,
                                                        allow_all_channels,
                                                        allow_all_users,
                                                        &allowed_channels,
                                                        &allowed_users,
                                                        &stt_config,
                                                        &dispatcher,
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
                                                    .is_some_and(|bot_uid| text_mentions_uid(msg_text, bot_uid));
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
                                                // Keyed on thread_ts when in a thread, else channel:ts.
                                                // Non-thread messages get a unique key per message, so the
                                                // counter never accumulates — intentional, because bot-to-bot
                                                // loops only happen inside threads.
                                                let turn_key = if let Some(thread_ts) = event["thread_ts"].as_str() {
                                                    thread_ts.to_string()
                                                } else {
                                                    format!("{}:{}", channel_id, event["ts"].as_str().unwrap_or(""))
                                                };
                                                // Classify under the lock (order-sensitive, kept in the read
                                                // loop), but run any warning send AFTER releasing it; holding
                                                // the tracker mutex across `chat.postMessage` would stall turn
                                                // tracking for every thread, not just this one.
                                                let turn_action = {
                                                    let mut tracker = bot_turns.lock().await;
                                                    if is_bot {
                                                        tracker.classify_bot_message(&turn_key)
                                                    } else {
                                                        if is_plain_user_message(subtype, msg_text) {
                                                            tracker.on_human_message(&turn_key);
                                                        }
                                                        TurnAction::Continue
                                                    }
                                                };
                                                match turn_action {
                                                    TurnAction::Continue => {}
                                                    TurnAction::SilentStop => continue,
                                                    TurnAction::WarnAndStop { severity, turns, user_message } => {
                                                        match severity {
                                                            TurnSeverity::Hard => warn!(channel_id, turns, "hard bot turn limit reached"),
                                                            TurnSeverity::Soft => info!(channel_id, turns, max = max_bot_turns, "soft bot turn limit reached"),
                                                        }
                                                        let channel_allowed = allow_all_channels
                                                            || allowed_channels.contains(channel_id);
                                                        if !is_own_bot_msg && channel_allowed {
                                                            let warn_channel = ChannelRef {
                                                                platform: "slack".into(),
                                                                channel_id: channel_id.to_string(),
                                                                thread_id: event["thread_ts"].as_str().map(|s| s.to_string()),
                                                                parent_id: None,
                                                                origin_event_id: None,
                                                            };
                                                            let adapter = adapter.clone();
                                                            tokio::spawn(async move {
                                                                if let Err(e) = adapter.send_message(&warn_channel, &user_message).await {
                                                                    warn!(error = %e, "failed to send bot turn limit warning");
                                                                }
                                                            });
                                                        }
                                                        continue;
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
                                                                let cap = MAX_CONSECUTIVE_BOT_TURNS;
                                                                let limit_str = std::cmp::min(cap + 1, 1000).to_string();
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
                                                                            if consecutive >= cap {
                                                                                warn!(channel_id, cap, "bot turn cap reached, ignoring");
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
                                                        let is_trusted = adapter
                                                            .trusted_bot_ids_contains(&trusted_bot_ids, event_bot_id)
                                                            .await;
                                                        if !is_trusted {
                                                            debug!(event_bot_id, "bot not in trusted_bot_ids, ignoring");
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

                                                // Dispatch to handle_message (per-thread serialization comes
                                                // from Dispatcher consumer task in batched mode and from
                                                // pool.with_connection in per-message mode).
                                                let team_id = envelope["payload"]["team_id"]
                                                    .as_str()
                                                    .unwrap_or("")
                                                    .to_string();
                                                let event = event.clone();
                                                let adapter = adapter.clone();
                                                let bot_token = bot_token.clone();
                                                let allowed_channels = allowed_channels.clone();
                                                let allowed_users = allowed_users.clone();
                                                let stt_config = stt_config.clone();
                                                let dispatcher = dispatcher.clone();
                                                tokio::spawn(async move {
                                                    handle_message(
                                                        &event,
                                                        &team_id,
                                                        &adapter,
                                                        &bot_token,
                                                        allow_all_channels,
                                                        allow_all_users,
                                                        &allowed_channels,
                                                        &allowed_users,
                                                        &stt_config,
                                                        &dispatcher,
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
                        _ = ping_interval.tick() => {
                            if socket_idle(
                                last_inbound.elapsed(),
                                std::time::Duration::from_secs(IDLE_TIMEOUT_SECS),
                            ) {
                                warn!(
                                    idle_secs = last_inbound.elapsed().as_secs(),
                                    "Slack Socket Mode idle past timeout (likely half-open), forcing reconnect"
                                );
                                break;
                            }
                            if let Err(e) = write.send(tungstenite::Message::Ping(Vec::new())).await {
                                warn!(error = %e, "Slack Socket Mode ping failed, reconnecting");
                                break;
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
                error!(err = %e, backoff = backoff_secs, "failed to connect to Slack Socket Mode, retrying");
            }
        }

        warn!(backoff = backoff_secs, "reconnecting to Slack Socket Mode");
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => { return Ok(()); }
        }
        backoff_secs = next_backoff(backoff_secs);
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
    team_id: &str,
    adapter: &Arc<SlackAdapter>,
    bot_token: &str,
    allow_all_channels: bool,
    allow_all_users: bool,
    allowed_channels: &HashSet<String>,
    allowed_users: &HashSet<String>,
    stt_config: &SttConfig,
    dispatcher: &Arc<crate::dispatch::Dispatcher>,
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
    let is_bot_msg =
        event["bot_id"].is_string() || event["subtype"].as_str() == Some("bot_message");
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
                origin_event_id: None,
            },
            message_id: ts.clone(),
        };
        let _ = adapter.add_reaction(&msg_ref, "🚫").await;
        return;
    }

    // Capture the native-streaming recipient for THIS turn, now that the sender has
    // passed the channel + user allow-list checks above (so denied/unauthorized
    // senders are never recorded). It rides on the per-turn BufferedMessage to
    // stream_begin — no shared thread cache, no cross-turn race. Real users only:
    // bot IDs (B...) are rejected by chat.startStream's recipient_user_id, and an
    // empty team_id would silently degrade, so we surface that.
    let stream_recipient = if is_bot_msg {
        None
    } else {
        if team_id.is_empty() {
            warn!("empty team_id; chat.startStream will degrade to post+edit");
        }
        Some((user_id.clone(), team_id.to_string()))
    };

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
    let mut echo_entries: Vec<crate::stt::EchoEntry> = Vec::new();
    let mut text_file_bytes: u64 = 0;
    let mut text_file_count: u32 = 0;
    let mut failed_image_files: Vec<String> = Vec::new();

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
                    match media::download_and_transcribe(
                        url,
                        filename,
                        mimetype,
                        size,
                        stt_config,
                        Some(bot_token),
                    )
                    .await
                    {
                        Some(transcript) => {
                            debug!(
                                filename,
                                chars = transcript.len(),
                                "voice transcript injected"
                            );
                            extra_blocks.insert(
                                0,
                                ContentBlock::Text {
                                    text: format!("[Voice message transcript]: {transcript}"),
                                },
                            );
                            echo_entries.push(crate::stt::EchoEntry::Success(transcript));
                        }
                        None => {
                            warn!(filename, "STT failed for voice attachment");
                            echo_entries.push(crate::stt::EchoEntry::Failed);
                        }
                    }
                } else {
                    debug!(filename, "skipping audio attachment (STT disabled)");
                    let msg_ref = MessageRef {
                        channel: ChannelRef {
                            platform: "slack".into(),
                            channel_id: channel_id.clone(),
                            thread_id: thread_ts.clone(),
                            parent_id: None,
                            origin_event_id: None,
                        },
                        message_id: ts.clone(),
                    };
                    let _ = adapter.add_reaction(&msg_ref, "🎤").await;
                }
            } else if media::is_text_file(filename, Some(mimetype)) {
                if text_file_count >= TEXT_FILE_COUNT_CAP {
                    debug!(
                        filename,
                        count = text_file_count,
                        "text file count cap reached, skipping"
                    );
                    continue;
                }
                // Pre-check with Slack-reported size as a fast path when the
                // field is populated. Slack can report `size == 0` for
                // externally-backed files, so this is advisory only — the
                // authoritative cap check happens after download using
                // `actual_bytes`.
                if size > 0 && text_file_bytes + size > TEXT_TOTAL_CAP {
                    debug!(
                        filename,
                        total = text_file_bytes,
                        "text attachments total exceeds 1MB cap, skipping remaining"
                    );
                    continue;
                }
                if let Some((block, actual_bytes)) =
                    media::download_and_read_text_file(url, filename, size, Some(bot_token)).await
                {
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
            } else {
                match media::download_and_encode_image(
                    url,
                    Some(mimetype),
                    filename,
                    size,
                    Some(bot_token),
                )
                .await
                {
                    Ok(block) => {
                        debug!(filename, "adding image attachment");
                        extra_blocks.push(block);
                    }
                    Err(media::MediaFetchError::NotAnImage) => {}
                    Err(media::MediaFetchError::SizeExceeded { actual, limit }) => {
                        warn!(filename, actual, limit, "image exceeds size limit");
                        failed_image_files.push(filename.to_string());
                    }
                    Err(
                        media::MediaFetchError::UnsupportedResponseType { .. }
                        | media::MediaFetchError::InvalidImageBody { .. },
                    ) => {
                        warn!(
                            filename,
                            "image validation failed; server may have returned non-image content"
                        );
                        failed_image_files.push(filename.to_string());
                    }
                    Err(media::MediaFetchError::ProcessingFailed(ref e)) => {
                        warn!(filename, error = %e, "image post-processing failed");
                        failed_image_files.push(filename.to_string());
                    }
                    Err(media::MediaFetchError::HttpStatus(status))
                        if status.is_client_error() =>
                    {
                        warn!(filename, %status, "image download denied");
                        failed_image_files.push(filename.to_string());
                    }
                    Err(e) => {
                        warn!(filename, error = %e, "image download failed");
                        failed_image_files.push(filename.to_string());
                    }
                }
            }
        }
    }

    // Notify user if any images couldn't be processed.
    if !failed_image_files.is_empty() {
        let warn_channel = ChannelRef {
            platform: "slack".into(),
            channel_id: channel_id.clone(),
            thread_id: thread_ts.clone().or_else(|| Some(ts.clone())),
            parent_id: None,
            origin_event_id: None,
        };
        let file_list = failed_image_files
            .iter()
            .map(|n| sanitize_slack_filename(n))
            .collect::<Vec<_>>()
            .join("`, `");
        let msg = format!(
            ":warning: I couldn't process the file(s) you shared (`{file_list}`). \
             This can happen when the bot lacks the `files:read` OAuth scope, \
             the file format isn't supported (PNG/JPEG/GIF/WebP only), \
             or the file is too large."
        );
        if let Err(e) = adapter.send_message(&warn_channel, &msg).await {
            warn!(error = %e, "failed to send image validation warning to user");
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
        timestamp: Some(crate::timestamp::slack_ts_to_iso8601(&ts)),
        message_id: Some(ts.clone()),
        receiver_id: bot_id.map(|id| id.to_string()),
    };

    let trigger_msg = MessageRef {
        channel: ChannelRef {
            platform: "slack".into(),
            channel_id: channel_id.clone(),
            thread_id: thread_ts.clone(),
            parent_id: None,
            origin_event_id: None,
        },
        message_id: ts.clone(),
    };

    // Determine thread: if already in a thread, continue it; otherwise start a new thread
    let thread_channel = ChannelRef {
        platform: "slack".into(),
        channel_id: channel_id.clone(),
        thread_id: Some(thread_ts.unwrap_or(ts)),
        parent_id: None,
        origin_event_id: None,
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
        thread_channel.thread_id.as_deref().is_some_and(|ts| {
            cache
                .get(ts)
                .is_some_and(|inst| inst.elapsed() < adapter.session_ttl)
        })
    } || thread_channel
        .thread_id
        .as_deref()
        .is_some_and(|ts| adapter.multibot_cache.is_multibot(ts));

    // Best-effort echo before the agent reply so the user can verify STT.
    crate::stt::post_echo(
        &adapter_dyn,
        &thread_channel,
        &trigger_msg,
        &echo_entries,
        stt_config,
    )
    .await;

    let thread_id = thread_channel
        .thread_id
        .as_deref()
        .unwrap_or(&thread_channel.channel_id);
    let thread_key = dispatcher.key("slack", thread_id, &sender.sender_id);
    let estimated_tokens = crate::dispatch::estimate_tokens(&prompt, &extra_blocks);
    let buf_msg = crate::dispatch::BufferedMessage {
        sender_json,
        sender_name: sender.sender_name.clone(),
        prompt,
        extra_blocks,
        trigger_msg,
        arrived_at: std::time::Instant::now(),
        estimated_tokens,
        other_bot_present,
        recipient: stream_recipient,
    };
    if let Err(e) = dispatcher
        .submit(thread_key, thread_channel, adapter_dyn, buf_msg)
        .await
    {
        error!("Slack dispatcher submit error: {e}");
    }
}

/// Strip all occurrences of the bot's own `<@BOT_UID>` or `<@BOT_UID|handle>` mention.
/// Other users' mentions stay intact so the LLM can @-mention them back.
/// If the bot UID isn't known, fall back to returning the text trimmed —
/// safer than stripping all mentions and losing user addressability.
fn resolve_slack_mentions(text: &str, bot_id: Option<&str>) -> String {
    let Some(id) = bot_id else {
        return text.trim().to_string();
    };
    let prefix = format!("<@{id}");
    let mut out = String::with_capacity(text.len());
    let mut s = text;
    while let Some(pos) = s.find(&prefix) {
        let after = &s[pos + prefix.len()..];
        match after.as_bytes().first() {
            Some(b'>') => {
                out.push_str(&s[..pos]);
                s = &after[1..];
            }
            Some(b'|') => {
                if let Some(close) = after.find('>') {
                    out.push_str(&s[..pos]);
                    s = &after[close + 1..];
                } else {
                    out.push_str(&s[..pos + prefix.len()]);
                    s = after;
                }
            }
            _ => {
                out.push_str(&s[..pos + prefix.len()]);
                s = after;
            }
        }
    }
    out.push_str(s);
    out.trim().to_string()
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

/// Strip MIME parameters so type-detection helpers see the bare media type.
/// Delegates to media::strip_mime_params (single source of truth).
/// Needed because Slack occasionally sends `text/plain; charset=utf-8` and
/// `media::is_text_file` expects the bare form.
fn strip_mime_params(mimetype: &str) -> &str {
    media::strip_mime_params(mimetype)
}

/// Sanitize a filename for safe embedding in a Slack mrkdwn message.
///
/// Ampersands (`&`), backticks (`` ` ``), and angle brackets (`<`, `>`) are escaped.
/// `&` is encoded as `&amp;` first because Slack decodes HTML entities before parsing
/// mrkdwn — a filename like `&lt;@here&gt;` would otherwise round-trip back to
/// `<@here>` and trigger a mention ping. Backticks and angle brackets are Slack
/// mrkdwn delimiters; without escaping, `<!here>` or `` `<@U123>` `` would render
/// as mentions or @-here pings.
pub(crate) fn sanitize_slack_filename(s: &str) -> String {
    s.replace('&', "&amp;").replace('`', "'").replace('<', "(").replace('>', ")")
}

/// Returns `true` if `text` contains a Slack user mention for `uid`.
///
/// Accepts both `<@U...>` (bare) and `<@U...|handle>` (labelled) wire forms.
/// Slack (and bots addressing peers) can emit the labelled form; `<@UID>` is
/// not a substring of `<@UID|handle>`, so a bare `contains("<@UID>")` silently
/// misses it.
fn text_mentions_uid(text: &str, uid: &str) -> bool {
    let prefix = format!("<@{uid}");
    text.match_indices(&prefix)
        .any(|(i, _)| matches!(text.as_bytes().get(i + prefix.len()), Some(b'>') | Some(b'|')))
}

fn bot_id_matches_trusted(
    trusted_bot_ids: &HashSet<String>,
    event_bot_id: &str,
    resolved_user_id: Option<&str>,
) -> bool {
    if event_bot_id.is_empty() {
        return false;
    }

    trusted_bot_ids.contains(event_bot_id)
        || resolved_user_id.is_some_and(|uid| trusted_bot_ids.contains(uid))
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

/// Slack caps a single Block Kit `markdown` block at 12,000 characters; we use
/// 11,900 to keep ~100 chars of headroom. Doubles as the Slack `message_limit`
/// so the router splits long replies into separate messages at the same bound
/// (one markdown block per message stays under the API cap).
const MARKDOWN_BLOCK_LIMIT: usize = 11_900;

/// True if a Slack API error indicates the `blocks` payload was rejected, so the
/// caller should retry text-only:
/// - `invalid_blocks` — workspace can't render the Block Kit `markdown` block
///   (malformed/unsupported payload).
/// - `msg_blocks_too_long` — content exceeds Slack's cumulative ~12k cap across
///   all `markdown` blocks in one message. Reachable by direct `send_message`
///   callers that bypass the router's `message_limit` pre-split (e.g. STT echo).
///
/// `invalid_arguments` is deliberately excluded — it's a Slack catch-all (bad
/// channel, missing/invalid `ts`, malformed `thread_ts`, …) and would trigger a
/// pointless text-only retry that fails identically.
///
/// Matches the Slack error *code* exactly (the trailing token of `api_post`'s
/// `"Slack API <method>: <code>"` message), not a substring of the message —
/// so a future code like `invalid_blocks_field` does not falsely match.
fn is_block_payload_rejected(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    let code = s.rsplit(": ").next().unwrap_or(s.as_str()).trim();
    code == "invalid_blocks" || code == "msg_blocks_too_long"
}

/// Build Block Kit `markdown` blocks from raw Markdown. Slack renders these
/// natively — real headings, lists, tables, blockquotes, and language-tagged
/// code fences — unlike the legacy `text` mrkdwn field, which flattens headings
/// to bold and cannot render tables. Long content is split at the block limit,
/// reusing `format::split_message` so code-fence balance is preserved.
///
/// Follow-up (non-blocking): `split_message` is not table-aware — a single
/// Markdown table exceeding `MARKDOWN_BLOCK_LIMIT` (11,900 chars) splits at line
/// boundaries, so continuation blocks lack the header/separator rows and render
/// as raw pipes. The 4000→11,900 bump makes this rare; a future improvement is
/// to re-emit the table header at the top of each continuation chunk.
fn build_markdown_blocks(content: &str) -> Vec<serde_json::Value> {
    let chunks = if content.len() <= MARKDOWN_BLOCK_LIMIT {
        vec![content.to_string()]
    } else {
        crate::format::split_message(content, MARKDOWN_BLOCK_LIMIT)
    };
    chunks
        .into_iter()
        .map(|chunk| serde_json::json!({ "type": "markdown", "text": chunk }))
        .collect()
}

/// Body for `chat.postMessage`: Block Kit `markdown` blocks (rich rendering)
/// plus a `text` fallback used for notifications and accessibility.
fn build_post_message_body(
    channel_id: &str,
    thread_ts: Option<&str>,
    content: &str,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "channel": channel_id,
        "blocks": build_markdown_blocks(content),
        "text": markdown_to_mrkdwn(content),
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = serde_json::Value::String(ts.to_string());
    }
    body
}

/// Body for `chat.update`: same Block Kit `markdown` blocks + `text` fallback.
fn build_update_body(channel_id: &str, ts: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "channel": channel_id,
        "ts": ts,
        "blocks": build_markdown_blocks(content),
        "text": markdown_to_mrkdwn(content),
    })
}

/// Text-only `chat.postMessage` body (no `blocks`) — degradation path when a
/// workspace rejects the Block Kit `markdown` block.
fn build_post_message_text_only(
    channel_id: &str,
    thread_ts: Option<&str>,
    content: &str,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "channel": channel_id,
        "text": markdown_to_mrkdwn(content),
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = serde_json::Value::String(ts.to_string());
    }
    body
}

/// Text-only `chat.update` body (no `blocks`) — see `build_post_message_text_only`.
fn build_update_text_only(channel_id: &str, ts: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "channel": channel_id,
        "ts": ts,
        "text": markdown_to_mrkdwn(content),
    })
}

/// Convert Markdown (as output by Claude Code) to Slack mrkdwn format.
/// Used for the `text` fallback field that accompanies Block Kit blocks
/// (shown in notification previews and to assistive tech).
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
    let text = BOLD_RE.replace_all(text, "\x01$1\x02"); // **bold** → \x01bold\x02
    let text = ITALIC_RE.replace_all(&text, "_${1}_"); // *italic* → _italic_
                                                       // Restore bold: \x01bold\x02 → *bold*
    let text = text.replace(['\x01', '\x02'], "*");
    let text = LINK_RE.replace_all(&text, "<$2|$1>"); // [text](url) → <url|text>
    let text = HEADING_RE.replace_all(&text, "*$1*"); // # heading → *heading*
    let text = CODE_BLOCK_LANG_RE.replace_all(&text, "```\n"); // ```rust → ```
    text.into_owned()
}

fn build_start_stream_body(channel: &str, thread_ts: &str, user_id: &str, team_id: &str) -> serde_json::Value {
    serde_json::json!({
        "channel": channel,
        "thread_ts": thread_ts,
        "recipient_user_id": user_id,
        "recipient_team_id": team_id,
    })
}

fn build_append_stream_body(channel: &str, ts: &str, delta: &str) -> serde_json::Value {
    serde_json::json!({
        "channel": channel,
        "ts": ts,
        "markdown_text": delta,
    })
}

fn build_set_status_body(channel_id: &str, thread_ts: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "channel_id": channel_id,
        "thread_ts": thread_ts,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- builder tests ---

    #[test]
    fn build_start_stream_body_has_recipient() {
        let b = build_start_stream_body("C1", "1700.1", "U2", "T3");
        assert_eq!(b["channel"], "C1");
        assert_eq!(b["thread_ts"], "1700.1");
        assert_eq!(b["recipient_user_id"], "U2");
        assert_eq!(b["recipient_team_id"], "T3");
    }

    #[test]
    fn build_append_stream_body_is_markdown_text_chunk() {
        let b = build_append_stream_body("C1", "1700.9", "hello");
        assert_eq!(b["channel"], "C1");
        assert_eq!(b["ts"], "1700.9");
        assert_eq!(b["markdown_text"], "hello");
    }

    #[test]
    fn build_set_status_body_shape() {
        let b = build_set_status_body("C1", "1700.1", "Thinking\u{2026}");
        assert_eq!(b["channel_id"], "C1");
        assert_eq!(b["thread_ts"], "1700.1");
        assert_eq!(b["status"], "Thinking\u{2026}");
    }

    #[tokio::test]
    async fn degraded_stream_append_accumulates() {
        let adapter = SlackAdapter::new("xoxb-test".into(), std::time::Duration::from_secs(60), AllowBots::Off, true, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);
        adapter.streams.lock().await.insert(
            "TS".into(),
            StreamEntry { active: false, degraded_buf: String::new() },
        );
        assert_eq!(adapter.accumulate_degraded("TS", "a").await.as_deref(), Some("a"));
        assert_eq!(adapter.accumulate_degraded("TS", "b").await.as_deref(), Some("ab"));
        // missing stream is not resurrected:
        assert_eq!(adapter.accumulate_degraded("MISSING", "x").await, None);
    }
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

    /// Labelled form of another user's mention (`<@UID|handle>`) is preserved.
    #[test]
    fn resolve_mentions_preserves_labelled_other_user_mention() {
        let out = resolve_slack_mentions("<@U1BOT> say hi to <@U2ALICE|alice>", Some("U1BOT"));
        assert_eq!(out, "say hi to <@U2ALICE|alice>");
    }

    /// Labelled form `<@UID|handle>` is stripped the same as bare form.
    #[test]
    fn resolve_mentions_strips_labelled_bot_mention() {
        let out = resolve_slack_mentions("<@U1BOT|my-bot> hello", Some("U1BOT"));
        assert_eq!(out, "hello");
    }

    /// Labelled form mid-sentence is stripped and surrounding text preserved.
    #[test]
    fn resolve_mentions_strips_labelled_mid_sentence() {
        let out = resolve_slack_mentions("please ask <@U1BOT|handle> to run", Some("U1BOT"));
        assert_eq!(out, "please ask  to run");
    }

    /// Mixed bare and labelled forms of the same UID in one string are both stripped.
    #[test]
    fn resolve_mentions_strips_mixed_bare_and_labelled() {
        let out = resolve_slack_mentions("<@U1BOT> and <@U1BOT|handle> run", Some("U1BOT"));
        assert_eq!(out, "and  run");
    }

    /// Malformed unclosed `<@UID|label` (no closing `>`) is preserved verbatim.
    #[test]
    fn resolve_mentions_malformed_unclosed_label_preserved() {
        let out = resolve_slack_mentions("ask <@U1BOT|nolabel to run", Some("U1BOT"));
        assert!(out.contains("<@U1BOT"));
    }

    #[test]
    fn resolve_mentions_preserves_longer_uid_prefix() {
        let out = resolve_slack_mentions("<@U1BOTX> hello", Some("U1BOT"));
        assert_eq!(out, "<@U1BOTX> hello");
    }

    // --- text_mentions_uid tests ---

    #[test]
    fn mentions_uid_bare_form() {
        assert!(text_mentions_uid("<@U123BOT> hello", "U123BOT"));
    }

    #[test]
    fn mentions_uid_labelled_form() {
        assert!(text_mentions_uid("<@U123BOT|my-bot> hello", "U123BOT"));
    }

    #[test]
    fn mentions_uid_labelled_form_mid_sentence() {
        assert!(text_mentions_uid("please ask <@U123BOT|handle> to run", "U123BOT"));
    }

    #[test]
    fn mentions_uid_no_match() {
        assert!(!text_mentions_uid("hello world", "U123BOT"));
    }

    #[test]
    fn mentions_uid_no_false_positive_on_uid_prefix() {
        assert!(!text_mentions_uid("<@U123BOT> hello", "U123"));
    }

    #[test]
    fn mentions_uid_second_mention_matches() {
        assert!(text_mentions_uid("<@U999OTHER> and <@U123BOT>", "U123BOT"));
    }

    #[test]
    fn mentions_uid_empty_label_form() {
        assert!(text_mentions_uid("<@U123BOT|> hello", "U123BOT"));
    }

    #[test]
    fn mentions_uid_truncated_no_closing_delimiter() {
        assert!(!text_mentions_uid("<@U123BOT", "U123BOT"));
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

    // --- sanitize_slack_filename tests ---

    #[test]
    fn sanitize_leaves_normal_filename_unchanged() {
        assert_eq!(sanitize_slack_filename("photo.png"), "photo.png");
        assert_eq!(sanitize_slack_filename("my file (1).jpg"), "my file (1).jpg");
    }

    #[test]
    fn sanitize_replaces_backtick() {
        assert_eq!(sanitize_slack_filename("file`name.png"), "file'name.png");
    }

    #[test]
    fn sanitize_replaces_angle_brackets() {
        // Angle brackets are Slack mrkdwn delimiters; they must not pass through.
        assert_eq!(sanitize_slack_filename("<@U123>"), "(@U123)");
        assert_eq!(sanitize_slack_filename("<!here>"), "(!here)");
    }

    #[test]
    fn sanitize_combined_injection_attempt() {
        // A filename constructed to inject a Slack @here ping.
        assert_eq!(
            sanitize_slack_filename("`<!here>`"),
            "'(!here)'"
        );
    }

    #[test]
    fn sanitize_escapes_ampersand_before_angle_brackets() {
        // Slack mrkdwn decodes HTML entities before markup parsing.
        // "&lt;@here&gt;" would round-trip back to "<@here>" and trigger a mention
        // ping if & is not escaped. The & must be escaped first so downstream
        // Slack entity decoding cannot reconstruct a mrkdwn delimiter.
        assert_eq!(sanitize_slack_filename("&lt;@here&gt;"), "&amp;lt;@here&amp;gt;");
        assert_eq!(sanitize_slack_filename("file&name.png"), "file&amp;name.png");
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

    // --- bot_id_matches_trusted tests ---

    #[test]
    fn trusted_bot_ids_accepts_raw_slack_bot_id() {
        let trusted = HashSet::from(["B123BOT".to_string()]);
        assert!(bot_id_matches_trusted(&trusted, "B123BOT", None));
    }

    #[test]
    fn trusted_bot_ids_accepts_resolved_bot_user_id() {
        let trusted = HashSet::from(["U123BOT".to_string()]);
        assert!(bot_id_matches_trusted(
            &trusted,
            "B123BOT",
            Some("U123BOT")
        ));
    }

    #[test]
    fn trusted_bot_ids_rejects_unknown_bot_when_resolution_fails() {
        let trusted = HashSet::from(["U123BOT".to_string()]);
        assert!(!bot_id_matches_trusted(&trusted, "B999BOT", None));
    }

    #[test]
    fn trusted_bot_ids_rejects_empty_event_bot_id() {
        let trusted = HashSet::from(["".to_string()]);
        assert!(!bot_id_matches_trusted(&trusted, "", None));
    }

    /// Per-thread streaming: ON by default, OFF when another bot is present (#534).
    #[test]
    fn streaming_per_thread() {
        let ttl = std::time::Duration::from_secs(300);
        let adapter = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Mentions, false, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);

        assert!(
            adapter.use_streaming(false),
            "should stream when no other bot"
        );
        assert!(
            !adapter.use_streaming(true),
            "should NOT stream when other bot present"
        );
    }

    #[tokio::test]
    async fn assistant_mode_gates_status_and_native_streaming() {
        let ttl = std::time::Duration::from_secs(60);
        // assistant_mode=true → status API on; native streaming on (no other bot),
        // off when another bot is present; post+edit streaming on regardless.
        let adapter = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Off, true, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);
        assert!(adapter.uses_assistant_status(), "assistant_mode enables status API");
        assert!(adapter.use_streaming(false), "post+edit streaming on when no other bot");
        assert!(adapter.uses_native_streaming(false), "native streaming on when no other bot");
        assert!(!adapter.uses_native_streaming(true), "other bot present disables native");
        // assistant_mode=false → no status API, no native streaming; post+edit still streams.
        let adapter2 = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Off, false, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);
        assert!(!adapter2.uses_assistant_status());
        assert!(adapter2.use_streaming(false), "post+edit streaming independent of assistant_mode");
        assert!(!adapter2.uses_native_streaming(false), "native streaming requires assistant_mode");

        // streaming=false → send-once: neither post+edit nor native, even alone.
        let adapter3 = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Off, true, crate::multibot_cache::MultibotCache::load("/dev/null".into()), false);
        assert!(!adapter3.use_streaming(false), "streaming=false forces send-once (no post+edit)");
        assert!(!adapter3.uses_native_streaming(false), "streaming=false disables native even with assistant_mode");
        assert!(adapter3.uses_assistant_status(), "streaming switch does not affect assistant status API");
    }

    /// chat.postMessage body carries Block Kit `markdown` blocks with the raw
    /// Markdown preserved (NOT downgraded), plus a `text` fallback and thread_ts.
    #[test]
    fn post_message_body_uses_raw_markdown_blocks() {
        let b = build_post_message_body("C1", Some("1700.1"), "## Heading\n- item");
        assert_eq!(b["channel"], "C1");
        assert_eq!(b["thread_ts"], "1700.1");
        assert_eq!(b["blocks"][0]["type"], "markdown");
        // Raw markdown preserved — heading is NOT flattened to `*Heading*`.
        assert_eq!(b["blocks"][0]["text"], "## Heading\n- item");
        assert!(b["text"].is_string(), "text fallback present for a11y/notifs");
    }

    /// thread_ts is omitted (top-level post) when the channel has no thread.
    #[test]
    fn post_message_body_omits_thread_ts_when_none() {
        let b = build_post_message_body("C1", None, "hi");
        assert!(b.get("thread_ts").is_none());
    }

    /// chat.update body also uses Block Kit `markdown` blocks with raw markdown.
    #[test]
    fn update_body_uses_raw_markdown_blocks() {
        let b = build_update_body("C1", "1700.9", "**bold**");
        assert_eq!(b["channel"], "C1");
        assert_eq!(b["ts"], "1700.9");
        assert_eq!(b["blocks"][0]["type"], "markdown");
        assert_eq!(b["blocks"][0]["text"], "**bold**");
    }

    /// Content over the per-block cap (11,900) splits into multiple markdown
    /// blocks, each within the limit. Assert on char count — `split_message`
    /// enforces `chars().count() <= limit`, not byte length.
    #[test]
    fn long_content_splits_into_multiple_markdown_blocks() {
        let big = "lorem ipsum dolor\n".repeat(1000); // > MARKDOWN_BLOCK_LIMIT
        assert!(big.chars().count() > MARKDOWN_BLOCK_LIMIT);
        let blocks = build_markdown_blocks(&big);
        assert!(blocks.len() >= 2, "should split into multiple blocks");
        for blk in &blocks {
            assert_eq!(blk["type"], "markdown");
            assert!(blk["text"].as_str().unwrap().chars().count() <= MARKDOWN_BLOCK_LIMIT);
        }
    }

    /// Regression for the long-table split: a Markdown table that overflows the
    /// old 4000 limit but fits the new 11,900 message_limit must stay in a single
    /// chunk, so it isn't split mid-table into raw pipe text.
    #[test]
    fn typical_long_table_stays_in_one_chunk() {
        let ttl = std::time::Duration::from_secs(300);
        let adapter = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Mentions, true, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);
        let limit = adapter.message_limit();
        assert_eq!(limit, MARKDOWN_BLOCK_LIMIT);
        let mut table = String::from("| col a | col b | col c |\n|---|---|---|\n");
        for i in 0..150 {
            table.push_str(&format!("| row {i} aaaa | bbbb {i} | cccc {i} |\n"));
        }
        assert!(table.chars().count() > 4000, "table must exceed old limit");
        assert!(table.chars().count() < limit, "but fit the new one");
        assert_eq!(
            crate::format::split_message(&table, limit).len(),
            1,
            "table within message_limit must not be split mid-table"
        );
    }

    /// Text-only fallback bodies carry `text` and no `blocks` — used when a
    /// workspace rejects the Block Kit markdown block.
    #[test]
    fn text_only_fallback_bodies_have_no_blocks() {
        let post = build_post_message_text_only("C1", Some("1700.1"), "## H\n- x");
        assert!(post.get("blocks").is_none());
        assert!(post["text"].is_string());
        assert_eq!(post["thread_ts"], "1700.1");
        let upd = build_update_text_only("C1", "1700.9", "**b**");
        assert!(upd.get("blocks").is_none());
        assert!(upd["text"].is_string());
    }

    /// Error classifier matches `invalid_blocks` (malformed/unsupported blocks)
    /// and `msg_blocks_too_long` (over the cumulative block cap) → degrade to
    /// text. `invalid_arguments` is a Slack catch-all and must NOT trigger a
    /// pointless text-only retry; unrelated errors are ignored too.
    #[test]
    fn detects_block_payload_rejected_errors() {
        assert!(is_block_payload_rejected(&anyhow!(
            "Slack API chat.postMessage: invalid_blocks"
        )));
        assert!(
            is_block_payload_rejected(&anyhow!("Slack API chat.postMessage: msg_blocks_too_long")),
            "oversize block payload should degrade to text-only"
        );
        assert!(
            !is_block_payload_rejected(&anyhow!("Slack API chat.update: invalid_arguments")),
            "invalid_arguments is a catch-all, not a block-rejection signal"
        );
        assert!(!is_block_payload_rejected(&anyhow!(
            "Slack API chat.postMessage: channel_not_found"
        )));
        // Exact error-code match, not substring: a future code that merely
        // contains `invalid_blocks` must NOT trigger a text-only retry.
        assert!(
            !is_block_payload_rejected(&anyhow!("Slack API chat.postMessage: invalid_blocks_field")),
            "must match the error code exactly, not as a substring"
        );
    }

    /// Slack opts into native table rendering (Block Kit markdown / markdown_text
    /// stream chunks), so the router skips the table→code-block conversion.
    #[test]
    fn slack_renders_native_tables() {
        let ttl = std::time::Duration::from_secs(300);
        let adapter = SlackAdapter::new("xoxb-test".into(), ttl, AllowBots::Mentions, true, crate::multibot_cache::MultibotCache::load("/dev/null".into()), true);
        assert!(adapter.renders_native_tables("slack"));
    }
}

#[cfg(test)]
mod socket_keepalive_tests {
    use super::{next_backoff, socket_idle, IDLE_TIMEOUT_SECS, MAX_BACKOFF_SECS};
    use std::time::Duration;

    /// Backoff doubles and caps, matching the gateway adapter (1,2,4,8,16,30,30…).
    #[test]
    fn backoff_doubles_then_caps() {
        let mut b = 1u64;
        let seq: Vec<u64> = (0..8)
            .map(|_| {
                let cur = b;
                b = next_backoff(b);
                cur
            })
            .collect();
        assert_eq!(seq, vec![1, 2, 4, 8, 16, MAX_BACKOFF_SECS, MAX_BACKOFF_SECS, MAX_BACKOFF_SECS]);
        assert_eq!(next_backoff(MAX_BACKOFF_SECS), MAX_BACKOFF_SECS);
    }

    /// A half-open socket (no inbound past the window) is detected; an active one
    /// (recent inbound, e.g. a Slack ping) is not. This is the deaf-socket guard.
    #[test]
    fn idle_detects_half_open_at_boundary() {
        let timeout = Duration::from_secs(IDLE_TIMEOUT_SECS);
        assert!(!socket_idle(Duration::from_secs(0), timeout));
        assert!(!socket_idle(Duration::from_secs(IDLE_TIMEOUT_SECS - 1), timeout));
        assert!(socket_idle(Duration::from_secs(IDLE_TIMEOUT_SECS), timeout));
        assert!(socket_idle(Duration::from_secs(IDLE_TIMEOUT_SECS + 10), timeout));
    }
}

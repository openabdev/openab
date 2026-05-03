use crate::schema::*;
use axum::extract::State;
use prost::Message as ProstMessage;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Timing-safe string comparison to prevent side-channel attacks on tokens.
fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

// ---------------------------------------------------------------------------
// Feishu WebSocket protobuf frame (pbbp2.Frame)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, ProstMessage)]
pub struct WsFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<WsHeader>,
    #[prost(string, optional, tag = "6")]
    pub payload_encoding: Option<String>,
    #[prost(string, optional, tag = "7")]
    pub payload_type: Option<String>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
    #[prost(string, optional, tag = "9")]
    pub log_id_new: Option<String>,
}

#[derive(Clone, PartialEq, ProstMessage)]
pub struct WsHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionMode {
    Websocket,
    Webhook,
}

#[derive(Debug, Clone)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    pub domain: String,
    pub connection_mode: ConnectionMode,
    pub webhook_path: String,
    pub verification_token: Option<String>,
    pub encrypt_key: Option<String>,
    pub allowed_groups: Vec<String>,
    pub allowed_users: Vec<String>,
    pub require_mention: bool,
    pub dedupe_ttl_secs: u64,
    pub message_limit: usize,
}

impl FeishuConfig {
    /// Build config from environment variables. Returns None if FEISHU_APP_ID
    /// is not set (adapter disabled).
    pub fn from_env() -> Option<Self> {
        let app_id = std::env::var("FEISHU_APP_ID").ok()?;
        let app_secret = std::env::var("FEISHU_APP_SECRET").ok().unwrap_or_default();
        if app_secret.is_empty() {
            warn!("FEISHU_APP_ID set but FEISHU_APP_SECRET is empty");
            return None;
        }
        let domain = std::env::var("FEISHU_DOMAIN").unwrap_or_else(|_| "feishu".into());
        let connection_mode = match std::env::var("FEISHU_CONNECTION_MODE")
            .unwrap_or_else(|_| "websocket".into())
            .to_lowercase()
            .as_str()
        {
            "webhook" => ConnectionMode::Webhook,
            _ => ConnectionMode::Websocket,
        };
        let webhook_path = std::env::var("FEISHU_WEBHOOK_PATH")
            .unwrap_or_else(|_| "/webhook/feishu".into());
        let verification_token = std::env::var("FEISHU_VERIFICATION_TOKEN").ok();
        let encrypt_key = std::env::var("FEISHU_ENCRYPT_KEY").ok();
        let allowed_groups = parse_csv("FEISHU_ALLOWED_GROUPS");
        let allowed_users = parse_csv("FEISHU_ALLOWED_USERS");
        let require_mention = std::env::var("FEISHU_REQUIRE_MENTION")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        let dedupe_ttl_secs = std::env::var("FEISHU_DEDUPE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let message_limit = std::env::var("FEISHU_MESSAGE_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000);

        Some(Self {
            app_id,
            app_secret,
            domain,
            connection_mode,
            webhook_path,
            verification_token,
            encrypt_key,
            allowed_groups,
            allowed_users,
            require_mention,
            dedupe_ttl_secs,
            message_limit,
        })
    }

    /// API base URL for the configured domain.
    pub fn api_base(&self) -> String {
        if self.domain == "lark" {
            "https://open.larksuite.com".into()
        } else {
            "https://open.feishu.cn".into()
        }
    }
}

fn parse_csv(var: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Feishu event types (im.message.receive_v1)
// ---------------------------------------------------------------------------

mod event_types {
    use super::*;

    #[derive(Debug, Deserialize)]
    pub struct FeishuEventEnvelope {
        pub header: Option<FeishuEventHeader>,
        pub event: Option<FeishuEventBody>,
        pub challenge: Option<String>,
        #[serde(rename = "type")]
        pub event_type_field: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuEventHeader {
        pub event_id: Option<String>,
        pub event_type: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuEventBody {
        pub sender: Option<FeishuSender>,
        pub message: Option<FeishuMessage>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuSender {
        pub sender_id: Option<FeishuSenderId>,
        pub sender_type: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuSenderId {
        pub open_id: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuMessage {
        pub message_id: Option<String>,
        pub chat_id: Option<String>,
        pub chat_type: Option<String>,
        pub message_type: Option<String>,
        pub content: Option<String>,
        pub mentions: Option<Vec<FeishuMention>>,
        pub root_id: Option<String>,
        pub parent_id: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuMention {
        pub key: Option<String>,
        pub id: Option<FeishuMentionId>,
        pub name: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub struct FeishuMentionId {
        pub open_id: Option<String>,
    }

    /// Parse a feishu im.message.receive_v1 event into a GatewayEvent.
    /// Returns None if the event should be skipped (non-text, bot message, etc).
    pub fn parse_message_event(
        envelope: &FeishuEventEnvelope,
        bot_open_id: Option<&str>,
        config: &FeishuConfig,
    ) -> Option<GatewayEvent> {
        let _header = envelope.header.as_ref()?;
        let event = envelope.event.as_ref()?;
        let msg = event.message.as_ref()?;
        let sender = event.sender.as_ref()?;

        if msg.message_type.as_deref() != Some("text") {
            return None;
        }
        // Skip bot messages (sender_type: "bot" or "app")
        if matches!(sender.sender_type.as_deref(), Some("bot") | Some("app")) {
            return None;
        }

        let sender_open_id = sender.sender_id.as_ref()?.open_id.as_deref()?;
        if let Some(bot_id) = bot_open_id {
            if sender_open_id == bot_id {
                return None;
            }
        }

        // User allowlist: if configured, only allow listed users
        if !config.allowed_users.is_empty()
            && !config.allowed_users.iter().any(|u| u == sender_open_id)
        {
            return None;
        }

        let chat_id = msg.chat_id.as_deref()?;
        let message_id = msg.message_id.as_deref()?;

        // Group allowlist: if configured, only allow listed groups
        let is_group = msg.chat_type.as_deref() != Some("p2p");
        if is_group
            && !config.allowed_groups.is_empty()
            && !config.allowed_groups.iter().any(|g| g == chat_id)
        {
            return None;
        }

        let content_json: serde_json::Value = msg.content.as_deref()
            .and_then(|s| serde_json::from_str(s).ok())?;
        let raw_text = content_json.get("text")?.as_str()?;
        if raw_text.trim().is_empty() {
            return None;
        }

        let (clean_text, mention_ids) = extract_mentions(
            raw_text,
            msg.mentions.as_deref().unwrap_or(&[]),
            bot_open_id,
        );
        if clean_text.trim().is_empty() {
            return None;
        }

        let channel_type = match msg.chat_type.as_deref() {
            Some("p2p") => "direct",
            _ => "group",
        };

        // Gateway-side mention gating: in groups, skip if require_mention
        // is true and bot is not mentioned
        if channel_type == "group" && config.require_mention {
            if let Some(bot_id) = bot_open_id {
                let bot_mentioned = mention_ids.iter().any(|id| id == bot_id);
                if !bot_mentioned {
                    return None;
                }
            }
        }

        let thread_id = msg.root_id.clone().or_else(|| msg.parent_id.clone());

        Some(GatewayEvent::new(
            "feishu",
            ChannelInfo {
                id: chat_id.to_string(),
                channel_type: channel_type.to_string(),
                thread_id,
            },
            SenderInfo {
                id: sender_open_id.to_string(),
                name: sender_open_id.to_string(),
                display_name: sender_open_id.to_string(),
                is_bot: false,
            },
            clean_text.trim(),
            message_id,
            mention_ids,
        ))
    }

    fn extract_mentions(
        raw_text: &str,
        mentions: &[FeishuMention],
        bot_open_id: Option<&str>,
    ) -> (String, Vec<String>) {
        let mut text = raw_text.to_string();
        let mut ids = Vec::new();
        for m in mentions {
            let open_id = m.id.as_ref().and_then(|id| id.open_id.as_deref());
            if let Some(oid) = open_id {
                ids.push(oid.to_string());
                if let Some(key) = m.key.as_deref() {
                    if bot_open_id == Some(oid) {
                        text = text.replacen(key, "", 1);
                    }
                }
            }
        }
        (text, ids)
    }
}

pub use event_types::*;

// ---------------------------------------------------------------------------
// Deduplication
// ---------------------------------------------------------------------------

pub struct DedupeCache {
    seen: std::sync::Mutex<HashMap<String, Instant>>,
    ttl_secs: u64,
    max_size: usize,
}

impl DedupeCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            seen: std::sync::Mutex::new(HashMap::new()),
            ttl_secs,
            max_size: 10_000,
        }
    }

    /// Returns true if this id was already seen (duplicate).
    pub fn is_duplicate(&self, id: &str) -> bool {
        let mut map = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        // Lazy sweep
        if map.len() >= self.max_size {
            map.retain(|_, ts| ts.elapsed().as_secs() < self.ttl_secs);
        }
        if let Some(ts) = map.get(id) {
            if ts.elapsed().as_secs() < self.ttl_secs {
                return true;
            }
        }
        map.insert(id.to_string(), Instant::now());
        false
    }
}

// ---------------------------------------------------------------------------
// Token cache
// ---------------------------------------------------------------------------

pub struct FeishuTokenCache {
    /// (token, created_at, ttl_secs)
    token: RwLock<Option<(String, Instant, u64)>>,
    api_base: String,
    app_id: String,
    app_secret: String,
}

/// Refresh margin: renew 5 minutes before expiry.
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

impl FeishuTokenCache {
    pub fn new(config: &FeishuConfig) -> Self {
        Self {
            token: RwLock::new(None),
            api_base: config.api_base(),
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
        }
    }

    /// Construct with explicit api_base (for tests).
    pub fn with_base(config: &FeishuConfig, api_base: &str) -> Self {
        Self {
            token: RwLock::new(None),
            api_base: api_base.to_string(),
            app_id: config.app_id.clone(),
            app_secret: config.app_secret.clone(),
        }
    }

    /// Get a valid tenant_access_token, refreshing if expired or missing.
    pub async fn get_token(&self, client: &reqwest::Client) -> anyhow::Result<String> {
        // Fast path: read lock
        {
            let guard = self.token.read().await;
            if let Some((ref tok, ref ts, ttl)) = *guard {
                if ts.elapsed().as_secs() < ttl.saturating_sub(TOKEN_REFRESH_MARGIN_SECS) {
                    return Ok(tok.clone());
                }
            }
        }
        // Slow path: write lock + refresh
        let mut guard = self.token.write().await;
        // Double-check after acquiring write lock
        if let Some((ref tok, ref ts, ttl)) = *guard {
            if ts.elapsed().as_secs() < ttl.saturating_sub(TOKEN_REFRESH_MARGIN_SECS) {
                return Ok(tok.clone());
            }
        }
        let (new_token, expire) = self.refresh(client).await?;
        *guard = Some((new_token.clone(), Instant::now(), expire));
        Ok(new_token)
    }

    async fn refresh(&self, client: &reqwest::Client) -> anyhow::Result<(String, u64)> {
        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.api_base
        );
        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("feishu token refresh request failed: {e}"))?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("feishu token refresh parse failed: {e}"))?;

        let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = body
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("feishu token refresh error: code={code} msg={msg} status={status}");
        }

        let expire = body.get("expire").and_then(|v| v.as_u64()).unwrap_or(7200);

        let token = body.get("tenant_access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("feishu token refresh: missing tenant_access_token"))?;

        Ok((token, expire))
    }
}

// ---------------------------------------------------------------------------
// Adapter (aggregated state)
// ---------------------------------------------------------------------------

pub struct FeishuAdapter {
    pub config: FeishuConfig,
    pub token_cache: Arc<FeishuTokenCache>,
    pub bot_open_id: Arc<RwLock<Option<String>>>,
    pub dedupe: Arc<DedupeCache>,
    pub rate_limiter: Arc<RateLimiter>,
    pub name_cache: Arc<std::sync::Mutex<HashMap<String, String>>>,
    pub client: reqwest::Client,
}

impl FeishuAdapter {
    pub fn new(config: FeishuConfig) -> Self {
        let token_cache = Arc::new(FeishuTokenCache::new(&config));
        let dedupe = Arc::new(DedupeCache::new(config.dedupe_ttl_secs));
        let rate_limiter = Arc::new(RateLimiter::new(60, 120));
        Self {
            config,
            token_cache,
            dedupe,
            rate_limiter,
            bot_open_id: Arc::new(RwLock::new(None)),
            name_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            client: reqwest::Client::new(),
        }
    }

    /// Resolve bot identity (open_id) via API. Called during startup for both
    /// WebSocket and webhook modes so mention gating works in either mode.
    pub async fn resolve_bot_identity(&self) {
        let token = match self.token_cache.get_token(&self.client).await {
            Ok(t) => t,
            Err(e) => {
                warn!(err = %e, "feishu bot identity lookup failed (token error), mention gating may not work");
                return;
            }
        };
        match get_bot_info(&self.client, &self.config.api_base(), &token).await {
            Ok(bot_id) => {
                info!(bot_open_id = %bot_id, "feishu bot identity resolved");
                *self.bot_open_id.write().await = Some(bot_id);
            }
            Err(e) => {
                warn!(err = %e, "feishu bot identity lookup failed, mention gating may not work");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket long connection
// ---------------------------------------------------------------------------

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, watch};

/// Get WebSocket endpoint URL from feishu API.
/// Note: This API uses AppID+AppSecret directly, not Bearer token.
async fn get_ws_endpoint(
    client: &reqwest::Client,
    api_base: &str,
    app_id: &str,
    app_secret: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/callback/ws/endpoint", api_base);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "AppID": app_id,
            "AppSecret": app_secret,
        }))
        .send()
        .await?;
    let body: serde_json::Value = resp.json().await?;
    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown");
        anyhow::bail!("feishu ws endpoint error: code={code} msg={msg}");
    }
    body.get("data")
        .and_then(|d| d.get("URL"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("feishu ws endpoint: missing URL"))
}

/// Get bot identity (open_id) via bot info API.
async fn get_bot_info(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/open-apis/bot/v3/info", api_base);
    let resp = client.get(&url).bearer_auth(token).send().await?;
    let body: serde_json::Value = resp.json().await?;
    body.get("bot")
        .and_then(|b| b.get("open_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("feishu bot info: missing open_id"))
}

/// Spawn the feishu WebSocket long-connection task.
/// Returns a JoinHandle that runs until shutdown_rx fires.
pub async fn start_websocket(
    adapter: &FeishuAdapter,
    event_tx: broadcast::Sender<String>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let token_cache = adapter.token_cache.clone();
    let bot_open_id_store = adapter.bot_open_id.clone();
    let dedupe = adapter.dedupe.clone();
    let config = adapter.config.clone();
    let client = adapter.client.clone();
    let name_cache = adapter.name_cache.clone();

    let handle = tokio::spawn(async move {
        let mut backoff_secs = 1u64;
        loop {
            let result = ws_connect_loop(
                &token_cache,
                &bot_open_id_store,
                &dedupe,
                &config,
                &client,
                &event_tx,
                &mut shutdown_rx,
                &name_cache,
            )
            .await;

            if *shutdown_rx.borrow() {
                info!("feishu websocket shutting down");
                break;
            }

            match result {
                Ok(()) => {
                    info!("feishu websocket disconnected, reconnecting...");
                    backoff_secs = 1;
                }
                Err(e) => {
                    tracing::error!(err = %e, backoff = backoff_secs, "feishu websocket error, reconnecting...");
                    backoff_secs = (backoff_secs * 2).min(120);
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                _ = shutdown_rx.changed() => { break; }
            }
        }
    });

    Ok(handle)
}

/// Single WebSocket connection lifecycle.
async fn ws_connect_loop(
    token_cache: &Arc<FeishuTokenCache>,
    bot_open_id_store: &Arc<RwLock<Option<String>>>,
    dedupe: &Arc<DedupeCache>,
    config: &FeishuConfig,
    client: &reqwest::Client,
    event_tx: &broadcast::Sender<String>,
    shutdown_rx: &mut watch::Receiver<bool>,
    name_cache: &Arc<std::sync::Mutex<HashMap<String, String>>>,
) -> anyhow::Result<()> {
    let api_base = config.api_base();

    // Refresh bot identity on each reconnect in case it was not resolved earlier
    if bot_open_id_store.read().await.is_none() {
        if let Ok(token) = token_cache.get_token(client).await {
            if let Ok(bot_id) = get_bot_info(client, &api_base, &token).await {
                info!(bot_open_id = %bot_id, "feishu bot identity resolved on reconnect");
                *bot_open_id_store.write().await = Some(bot_id);
            }
        }
    }

    let ws_url = get_ws_endpoint(client, &api_base, &config.app_id, &config.app_secret).await?;
    info!(url = %ws_url, "feishu websocket connecting");

    let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    info!("feishu websocket connected");

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        handle_ws_message(
                            &text, bot_open_id_store, dedupe, config, event_tx,
                            name_cache, token_cache, client,
                        ).await;
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                        let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await;
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("websocket error: {e}"));
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => {
                        match WsFrame::decode(data.as_ref()) {
                            Ok(frame) => {
                                // method=1 is data frame (events), method=0 is control
                                if frame.method == 1 {
                                    if let Some(ref payload) = frame.payload {
                                        if let Ok(text) = String::from_utf8(payload.clone()) {
                                            handle_ws_message(
                                                &text, bot_open_id_store, dedupe, config, event_tx,
                                                name_cache, token_cache, client,
                                            ).await;
                                        }
                                    }
                                    // Send ACK: echo frame back with {"code":200} payload
                                    let mut ack = frame.clone();
                                    ack.payload = Some(b"{\"code\":200}".to_vec());
                                    let ack_bytes = ack.encode_to_vec();
                                    let _ = ws_tx.send(
                                        tokio_tungstenite::tungstenite::Message::Binary(ack_bytes.into())
                                    ).await;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(err = %e, len = data.len(), "feishu ws protobuf decode failed");
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ = shutdown_rx.changed() => {
                let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                return Ok(());
            }
        }
    }
}

/// Process a single WebSocket text message.
async fn handle_ws_message(
    text: &str,
    bot_open_id_store: &Arc<RwLock<Option<String>>>,
    dedupe: &Arc<DedupeCache>,
    config: &FeishuConfig,
    event_tx: &broadcast::Sender<String>,
    name_cache: &Arc<std::sync::Mutex<HashMap<String, String>>>,
    token_cache: &Arc<FeishuTokenCache>,
    client: &reqwest::Client,
) {
    let envelope: FeishuEventEnvelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Handle challenge frame (Feishu may send this in WS mode for verification)
    if let Some(ref challenge) = envelope.challenge {
        tracing::debug!(challenge = %challenge, "feishu ws challenge received (ignored in WS mode)");
        return;
    }

    // Debug: log sender_type for diagnosing bot-to-bot loops
    if let Some(ref event) = envelope.event {
        if let Some(ref sender) = event.sender {
            tracing::debug!(
                sender_type = ?sender.sender_type,
                sender_id = ?sender.sender_id.as_ref().and_then(|s| s.open_id.as_deref()),
                "feishu ws event sender"
            );
        }
    }

    // Dedupe by event_id
    if let Some(ref header) = envelope.header {
        if let Some(ref event_id) = header.event_id {
            if dedupe.is_duplicate(event_id) {
                return;
            }
        }
    }

    let bot_id = bot_open_id_store.read().await;
    let bot_id_ref = bot_id.as_deref();

    if let Some(mut gateway_event) = parse_message_event(&envelope, bot_id_ref, config) {
        // Also dedupe by message_id
        if dedupe.is_duplicate(&gateway_event.message_id) {
            return;
        }
        // Resolve sender display name (lazy, cached)
        let name = resolve_user_name(
            &gateway_event.sender.id, name_cache, token_cache, client, &config.api_base(),
        ).await;
        gateway_event.sender.name = name.clone();
        gateway_event.sender.display_name = name;
        let json = serde_json::to_string(&gateway_event).unwrap();
        info!(
            channel = %gateway_event.channel.id,
            sender = %gateway_event.sender.id,
            "feishu → gateway"
        );
        let _ = event_tx.send(json);
    }
}

/// Resolve user display name from open_id via Contact API, with caching.
async fn resolve_user_name(
    open_id: &str,
    name_cache: &Arc<std::sync::Mutex<HashMap<String, String>>>,
    token_cache: &Arc<FeishuTokenCache>,
    client: &reqwest::Client,
    api_base: &str,
) -> String {
    {
        let cache = name_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(name) = cache.get(open_id) {
            return name.clone();
        }
    }
    let token = match token_cache.get_token(client).await {
        Ok(t) => t,
        Err(_) => return open_id.to_string(),
    };
    let url = format!(
        "{}/open-apis/contact/v3/users/{}?user_id_type=open_id",
        api_base, open_id
    );
    let resolved = match client.get(&url).bearer_auth(&token).send().await {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            body.pointer("/data/user/name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        Err(_) => None,
    };
    // Only cache successful resolutions — don't cache fallback open_id
    // so retries can succeed after permissions are granted.
    if let Some(ref name) = resolved {
        let mut cache = name_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() < 10_000 {
            cache.insert(open_id.to_string(), name.clone());
        }
    }
    resolved.unwrap_or_else(|| open_id.to_string())
}

// ---------------------------------------------------------------------------
// Send message
// ---------------------------------------------------------------------------

/// Send a text message to a feishu chat_id.
/// Returns the sent message_id on success (for self-echo dedupe), None on failure.
pub async fn send_text_message(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    chat_id: &str,
    text: &str,
) -> Option<String> {
    let url = format!(
        "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
        api_base
    );
    let content = serde_json::json!({"text": text}).to_string();
    let body = serde_json::json!({
        "receive_id": chat_id,
        "msg_type": "text",
        "content": content,
    });

    match client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                let msg_id = match resp.json::<serde_json::Value>().await {
                    Ok(body) => body
                        .pointer("/data/message_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    Err(e) => {
                        warn!(chat_id = %chat_id, err = %e, "feishu 200 response not valid JSON, self-echo dedupe will be skipped");
                        None
                    }
                };
                info!(chat_id = %chat_id, message_id = ?msg_id, "feishu message sent");
                msg_id
            } else {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                tracing::error!(status = %status, body = %text, "feishu send message failed");
                None
            }
        }
        Err(e) => {
            tracing::error!(err = %e, "feishu send message request failed");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Reactions (emoji on original message)
// ---------------------------------------------------------------------------

/// Map OAB emoji to feishu reaction_type. Feishu uses string keys like "THUMBSUP".
fn emoji_to_feishu_reaction(emoji: &str) -> Option<&'static str> {
    match emoji {
        "👀" => Some("EYES"),
        "🤔" => Some("THINKING"),
        "🔥" => Some("FIRE"),
        "👨\u{200d}💻" => Some("TECHNOLOGIST"),
        "⚡" => Some("LIGHTNING"),
        "🆗" => Some("OK"),
        "👍" => Some("THUMBSUP"),
        "😱" => Some("SCREAM"),
        _ => None,
    }
}

async fn add_reaction(adapter: &FeishuAdapter, message_id: &str, emoji: &str) {
    let reaction_type = match emoji_to_feishu_reaction(emoji) {
        Some(r) => r,
        None => {
            tracing::debug!(emoji = %emoji, "feishu: no mapping for reaction emoji");
            return;
        }
    };
    let token = match adapter.token_cache.get_token(&adapter.client).await {
        Ok(t) => t,
        Err(e) => { tracing::error!(err = %e, "feishu: cannot get token for reaction"); return; }
    };
    let url = format!(
        "{}/open-apis/im/v1/messages/{}/reactions",
        adapter.config.api_base(), message_id
    );
    let _ = adapter.client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"reaction_type": {"emoji_type": reaction_type}}))
        .send()
        .await
        .map_err(|e| tracing::error!(err = %e, "feishu add_reaction failed"));
}

async fn remove_reaction(adapter: &FeishuAdapter, message_id: &str, emoji: &str) {
    let reaction_type = match emoji_to_feishu_reaction(emoji) {
        Some(r) => r,
        None => return,
    };
    let token = match adapter.token_cache.get_token(&adapter.client).await {
        Ok(t) => t,
        Err(e) => { tracing::error!(err = %e, "feishu: cannot get token for reaction"); return; }
    };
    // Feishu remove reaction needs reaction_id. Simpler approach: delete by type.
    // GET reactions, find matching, DELETE by id.
    let list_url = format!(
        "{}/open-apis/im/v1/messages/{}/reactions?reaction_type={}",
        adapter.config.api_base(), message_id, reaction_type
    );
    let resp = match adapter.client.get(&list_url).bearer_auth(&token).send().await {
        Ok(r) => r,
        Err(_) => return,
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return,
    };
    // Find our bot's reaction_id
    if let Some(items) = body.pointer("/data/items").and_then(|v| v.as_array()) {
        let bot_id = adapter.bot_open_id.read().await;
        for item in items {
            let is_ours = item.pointer("/operator/operator_id/open_id")
                .and_then(|v| v.as_str()) == bot_id.as_deref();
            if is_ours {
                if let Some(reaction_id) = item.get("reaction_id").and_then(|v| v.as_str()) {
                    let del_url = format!(
                        "{}/open-apis/im/v1/messages/{}/reactions/{}",
                        adapter.config.api_base(), message_id, reaction_id
                    );
                    let _ = adapter.client.delete(&del_url).bearer_auth(&token).send().await;
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reply handler
// ---------------------------------------------------------------------------

pub async fn handle_reply(
    reply: &GatewayReply,
    adapter: &FeishuAdapter,
) {
    // Handle reactions — add/remove emoji on the original message
    if let Some(ref cmd) = reply.command {
        match cmd.as_str() {
            "add_reaction" => {
                add_reaction(adapter, &reply.reply_to, &reply.content.text).await;
                return;
            }
            "remove_reaction" => {
                remove_reaction(adapter, &reply.reply_to, &reply.content.text).await;
                return;
            }
            "create_topic" | "set_reaction" => {
                tracing::debug!(command = %cmd, "feishu: skipping unsupported command");
                return;
            }
            _ => {}
        }
    }

    let token = match adapter.token_cache.get_token(&adapter.client).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(err = %e, "feishu: cannot get token for reply");
            return;
        }
    };

    let api_base = adapter.config.api_base();
    let text = &reply.content.text;
    let limit = adapter.config.message_limit;

    // Split long messages; store sent message_ids in dedupe to prevent
    // self-echo (Feishu pushes bot's own messages back via WebSocket)
    if text.len() <= limit {
        if let Some(msg_id) = send_text_message(&adapter.client, &api_base, &token, &reply.channel.id, text).await {
            adapter.dedupe.is_duplicate(&msg_id);
        }
    } else {
        for chunk in split_text(text, limit) {
            if let Some(msg_id) = send_text_message(&adapter.client, &api_base, &token, &reply.channel.id, chunk).await {
                adapter.dedupe.is_duplicate(&msg_id);
            }
        }
    }
}

/// Split text into chunks of at most `limit` bytes, breaking at newline or
/// space boundaries when possible. Safe for multi-byte UTF-8 (e.g. Chinese).
fn split_text(text: &str, limit: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        if start + limit >= text.len() {
            chunks.push(&text[start..]);
            break;
        }
        // Find a char-safe boundary at or before start + limit
        let mut end = start + limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        // Try to break at a newline or space within the last 200 bytes.
        // search_start must also be on a char boundary to avoid panic.
        let mut search_start = if end > start + 200 { end - 200 } else { start };
        while search_start < end && !text.is_char_boundary(search_start) {
            search_start += 1;
        }
        let break_at = text[search_start..end]
            .rfind('\n')
            .or_else(|| text[search_start..end].rfind(' '))
            .map(|pos| search_start + pos + 1)
            .unwrap_or(end);
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

// ---------------------------------------------------------------------------
// Webhook handler
// ---------------------------------------------------------------------------

/// Max webhook body size: 1 MB
const WEBHOOK_BODY_LIMIT: usize = 1_048_576;

/// Simple per-IP rate limiter state.
pub struct RateLimiter {
    counts: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    window_secs: u64,
    max_requests: u64,
}

impl RateLimiter {
    pub fn new(window_secs: u64, max_requests: u64) -> Self {
        Self {
            counts: std::sync::Mutex::new(HashMap::new()),
            window_secs,
            max_requests,
        }
    }

    /// Returns true if the request should be rejected (rate exceeded).
    pub fn check(&self, key: &str) -> bool {
        let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        // Lazy cleanup
        if map.len() > 4096 {
            map.retain(|_, (_, ts)| ts.elapsed().as_secs() < self.window_secs);
        }
        let entry = map.entry(key.to_string()).or_insert((0, Instant::now()));
        if entry.1.elapsed().as_secs() >= self.window_secs {
            *entry = (1, Instant::now());
            false
        } else {
            entry.0 += 1;
            entry.0 > self.max_requests
        }
    }
}

/// Verify webhook signature: SHA256(timestamp + nonce + encrypt_key + body).
fn verify_signature(
    timestamp: &str,
    nonce: &str,
    encrypt_key: &str,
    body: &[u8],
    expected_sig: &str,
) -> bool {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(encrypt_key.as_bytes());
    hasher.update(body);
    let result = format!("{:x}", hasher.finalize());
    constant_time_eq(&result, expected_sig)
}

/// Decrypt AES-CBC encrypted event body.
/// Feishu uses AES-256-CBC with SHA256(encrypt_key) as key, first 16 bytes of
/// ciphertext as IV.
fn decrypt_event(encrypt_key: &str, encrypted: &str) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let key = Sha256::digest(encrypt_key.as_bytes());
    let cipher_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        encrypted,
    )
    .map_err(|e| anyhow::anyhow!("base64 decode failed: {e}"))?;

    if cipher_bytes.len() < 16 {
        anyhow::bail!("encrypted data too short");
    }

    let iv = &cipher_bytes[..16];
    let ciphertext = &cipher_bytes[16..];

    // AES-256-CBC decrypt
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let decryptor = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| anyhow::anyhow!("aes init failed: {e}"))?;

    let mut buf = ciphertext.to_vec();
    let plaintext = decryptor
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| anyhow::anyhow!("aes decrypt failed: {e}"))?;

    String::from_utf8(plaintext.to_vec())
        .map_err(|e| anyhow::anyhow!("decrypted data not utf8: {e}"))
}

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let feishu = match state.feishu.as_ref() {
        Some(f) => f,
        None => return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    // Body size limit
    if body.len() > WEBHOOK_BODY_LIMIT {
        warn!(size = body.len(), "feishu webhook body too large");
        return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    // Rate limit (by X-Forwarded-For or fallback)
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    if feishu.rate_limiter.check(ip) {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded")
            .into_response();
    }

    // Signature verification (if encrypt_key configured)
    if let Some(ref encrypt_key) = feishu.config.encrypt_key {
        let sig = headers
            .get("x-lark-signature")
            .and_then(|v| v.to_str().ok());
        let timestamp = headers
            .get("x-lark-request-timestamp")
            .and_then(|v| v.to_str().ok());
        let nonce = headers
            .get("x-lark-request-nonce")
            .and_then(|v| v.to_str().ok());

        match (sig, timestamp, nonce) {
            (Some(sig), Some(ts), Some(nonce)) => {
                if !verify_signature(ts, nonce, encrypt_key, &body, sig) {
                    warn!("feishu webhook rejected: invalid signature");
                    return axum::http::StatusCode::UNAUTHORIZED.into_response();
                }
            }
            _ => {
                warn!("feishu webhook rejected: missing signature headers");
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
        }
    } else {
        warn!("FEISHU_ENCRYPT_KEY not configured — webhook signature verification is SKIPPED (insecure)");
    }

    // Parse body — may be encrypted
    let event_json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!(err = %e, "feishu webhook parse error");
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
    };

    // Handle encrypted events
    let event_json = if let Some(encrypted) = event_json.get("encrypt").and_then(|v| v.as_str()) {
        let encrypt_key = match feishu.config.encrypt_key.as_deref() {
            Some(k) => k,
            None => {
                warn!("feishu webhook: encrypted event but no FEISHU_ENCRYPT_KEY configured");
                return axum::http::StatusCode::BAD_REQUEST.into_response();
            }
        };
        match decrypt_event(encrypt_key, encrypted) {
            Ok(decrypted) => match serde_json::from_str(&decrypted) {
                Ok(v) => v,
                Err(e) => {
                    warn!(err = %e, "feishu webhook: decrypted event parse error");
                    return axum::http::StatusCode::BAD_REQUEST.into_response();
                }
            },
            Err(e) => {
                warn!(err = %e, "feishu webhook: decrypt failed");
                return axum::http::StatusCode::BAD_REQUEST.into_response();
            }
        }
    } else {
        event_json
    };

    // URL verification challenge
    if event_json.get("challenge").is_some() {
        // Verify token if configured
        if let Some(ref expected_token) = feishu.config.verification_token {
            let token = event_json.get("token").and_then(|v| v.as_str());
            match token {
                Some(t) if constant_time_eq(t, expected_token) => {}
                _ => {
                    warn!("feishu webhook: URL verification token mismatch");
                    return axum::http::StatusCode::UNAUTHORIZED.into_response();
                }
            }
        }
        let challenge = event_json["challenge"].as_str().unwrap_or("");
        return axum::Json(serde_json::json!({"challenge": challenge})).into_response();
    }

    // Verification token check for regular events
    if let Some(ref expected_token) = feishu.config.verification_token {
        let token = event_json
            .pointer("/header/token")
            .or_else(|| event_json.get("token"))
            .and_then(|v| v.as_str());
        match token {
            Some(t) if constant_time_eq(t, expected_token) => {}
            _ => {
                warn!("feishu webhook rejected: invalid verification token");
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
        }
    }

    // Parse as event envelope
    let envelope: FeishuEventEnvelope = match serde_json::from_value(event_json) {
        Ok(e) => e,
        Err(e) => {
            warn!(err = %e, "feishu webhook: event envelope parse error");
            return axum::http::StatusCode::OK.into_response();
        }
    };

    // Dedupe + parse + broadcast (same logic as WebSocket handler)
    if let Some(ref header) = envelope.header {
        if let Some(ref event_id) = header.event_id {
            if feishu.dedupe.is_duplicate(event_id) {
                return axum::http::StatusCode::OK.into_response();
            }
        }
    }

    let bot_id = feishu.bot_open_id.read().await;
    let bot_id_ref = bot_id.as_deref();

    if let Some(mut gateway_event) = parse_message_event(&envelope, bot_id_ref, &feishu.config) {
        if !feishu.dedupe.is_duplicate(&gateway_event.message_id) {
            let name = resolve_user_name(
                &gateway_event.sender.id, &feishu.name_cache, &feishu.token_cache,
                &feishu.client, &feishu.config.api_base(),
            ).await;
            gateway_event.sender.name = name.clone();
            gateway_event.sender.display_name = name;
            let json = serde_json::to_string(&gateway_event).unwrap();
            info!(
                channel = %gateway_event.channel.id,
                sender = %gateway_event.sender.id,
                "feishu webhook → gateway"
            );
            let _ = state.event_tx.send(json);
        }
    }

    axum::http::StatusCode::OK.into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> FeishuConfig {
        FeishuConfig {
            app_id: "cli_test".into(),
            app_secret: "secret_test".into(),
            domain: "feishu".into(),
            connection_mode: ConnectionMode::Websocket,
            webhook_path: "/webhook/feishu".into(),
            verification_token: None,
            encrypt_key: None,
            allowed_groups: vec![],
            allowed_users: vec![],
            require_mention: true,
            dedupe_ttl_secs: 300,
            message_limit: 4000,
        }
    }

    // --- Token tests ---

    #[tokio::test]
    async fn token_refresh_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .and(body_json(serde_json::json!({
                "app_id": "cli_test",
                "app_secret": "secret_test",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "msg": "ok",
                "tenant_access_token": "t-test-token-123",
                "expire": 7200
            })))
            .expect(1)
            .mount(&server)
            .await;

        let config = test_config();
        let cache = FeishuTokenCache::with_base(&config, &server.uri());
        let client = reqwest::Client::new();

        let token = cache.get_token(&client).await.unwrap();
        assert_eq!(token, "t-test-token-123");

        // Second call should use cache, not hit server again (expect(1) above)
        let token2 = cache.get_token(&client).await.unwrap();
        assert_eq!(token2, "t-test-token-123");
    }

    #[tokio::test]
    async fn token_refresh_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 10003,
                "msg": "invalid app_secret",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let config = test_config();
        let cache = FeishuTokenCache::with_base(&config, &server.uri());
        let client = reqwest::Client::new();

        let err = cache.get_token(&client).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("10003"), "error should contain code: {msg}");
        assert!(
            !msg.contains("secret_test"),
            "error must not leak secret: {msg}"
        );
    }

    // --- Send message tests ---

    #[tokio::test]
    async fn send_text_message_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .and(header("authorization", "Bearer t-tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "msg": "success",
                "data": {"message_id": "om_test123"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let msg_id = send_text_message(&client, &server.uri(), "t-tok", "oc_chat1", "hello").await;
        assert_eq!(msg_id.as_deref(), Some("om_test123"));
    }

    #[tokio::test]
    async fn send_text_message_api_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let msg_id = send_text_message(&client, &server.uri(), "t-tok", "oc_chat1", "hello").await;
        assert!(msg_id.is_none());
    }

    #[tokio::test]
    async fn send_text_message_invalid_json_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let msg_id = send_text_message(&client, &server.uri(), "t-tok", "oc_chat1", "hello").await;
        assert!(msg_id.is_none());
    }

    #[tokio::test]
    async fn send_text_message_missing_message_id_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "msg": "success",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let msg_id = send_text_message(&client, &server.uri(), "t-tok", "oc_chat1", "hello").await;
        assert!(msg_id.is_none());
    }

    // --- Split text tests ---

    #[test]
    fn split_text_short() {
        let chunks = split_text("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_text_exact_limit() {
        let text = "a".repeat(100);
        let chunks = split_text(&text, 100);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn split_text_chinese_utf8_safe() {
        // Each Chinese char is 3 bytes. 20 chars = 60 bytes.
        // Limit 10 would land mid-char without boundary check.
        let text = "你好世界測試飛書中文聊天消息分割安全驗證完成";
        let chunks = split_text(text, 10);
        assert!(chunks.len() > 1);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_text_search_start_char_boundary() {
        // Regression: search_start = end - 200 could land mid-char.
        // 300 Chinese chars (900 bytes) with limit=500 forces search_start
        // into the middle of multi-byte chars.
        let text: String = "飛書".repeat(150); // 300 chars, 900 bytes
        let chunks = split_text(&text, 500);
        assert!(chunks.len() >= 2);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_text_long_breaks_at_newline() {
        let text = format!("{}\n{}", "a".repeat(50), "b".repeat(50));
        let chunks = split_text(&text, 60);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
    }

    // --- Event parsing tests ---

    fn make_envelope(
        chat_type: &str,
        text: &str,
        sender_open_id: &str,
        mentions: Option<Vec<FeishuMention>>,
    ) -> FeishuEventEnvelope {
        FeishuEventEnvelope {
            header: Some(FeishuEventHeader {
                event_id: Some("evt_test".into()),
                event_type: Some("im.message.receive_v1".into()),
            }),
            event: Some(FeishuEventBody {
                sender: Some(FeishuSender {
                    sender_id: Some(FeishuSenderId {
                        open_id: Some(sender_open_id.into()),
                    }),
                    sender_type: Some("user".into()),
                }),
                message: Some(FeishuMessage {
                    message_id: Some("om_msg1".into()),
                    chat_id: Some("oc_chat1".into()),
                    chat_type: Some(chat_type.into()),
                    message_type: Some("text".into()),
                    content: Some(serde_json::json!({"text": text}).to_string()),
                    mentions,
                    root_id: None,
                    parent_id: None,
                }),
            }),
            challenge: None,
            event_type_field: None,
        }
    }

    #[test]
    fn parse_dm_text() {
        let env = make_envelope("p2p", "hello", "ou_user1", None);
        let cfg = test_config();
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg).unwrap();
        assert_eq!(evt.platform, "feishu");
        assert_eq!(evt.channel.channel_type, "direct");
        assert_eq!(evt.channel.id, "oc_chat1");
        assert_eq!(evt.sender.id, "ou_user1");
        assert_eq!(evt.content.text, "hello");
        assert!(evt.mentions.is_empty());
    }

    #[test]
    fn parse_group_with_bot_mention() {
        let mentions = vec![FeishuMention {
            key: Some("@_user_1".into()),
            id: Some(FeishuMentionId {
                open_id: Some("ou_bot".into()),
            }),
            name: Some("Bot".into()),
        }];
        let env = make_envelope("group", "@_user_1 explain VPC", "ou_user1", Some(mentions));
        let cfg = test_config();
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg).unwrap();
        assert_eq!(evt.channel.channel_type, "group");
        assert_eq!(evt.content.text, "explain VPC");
        assert_eq!(evt.mentions, vec!["ou_bot"]);
    }

    #[test]
    fn parse_group_without_mention_filtered() {
        let env = make_envelope("group", "just chatting", "ou_user1", None);
        let cfg = test_config(); // require_mention = true
        // Gateway-side mention gating: group message without bot mention is filtered
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_group_without_mention_allowed_when_disabled() {
        let env = make_envelope("group", "just chatting", "ou_user1", None);
        let mut cfg = test_config();
        cfg.require_mention = false;
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg);
        assert!(evt.is_some());
    }

    #[test]
    fn parse_skips_bot_sender() {
        let mut env = make_envelope("p2p", "hello", "ou_bot", None);
        env.event.as_mut().unwrap().sender.as_mut().unwrap().sender_type = Some("bot".into());
        let cfg = test_config();
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_skips_empty_text() {
        let env = make_envelope("p2p", "  ", "ou_user1", None);
        let cfg = test_config();
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_skips_non_text_message() {
        let mut env = make_envelope("p2p", "hello", "ou_user1", None);
        env.event.as_mut().unwrap().message.as_mut().unwrap().message_type = Some("image".into());
        let cfg = test_config();
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_skips_self_message() {
        let env = make_envelope("p2p", "hello", "ou_bot", None);
        let cfg = test_config();
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    // --- Dedupe tests ---

    #[test]
    fn dedupe_first_is_not_duplicate() {
        let cache = DedupeCache::new(300);
        assert!(!cache.is_duplicate("msg_1"));
    }

    #[test]
    fn dedupe_second_is_duplicate() {
        let cache = DedupeCache::new(300);
        assert!(!cache.is_duplicate("msg_1"));
        assert!(cache.is_duplicate("msg_1"));
    }

    // --- Webhook security tests ---

    #[test]
    fn verify_signature_correct() {
        use sha2::{Digest, Sha256};
        let ts = "1234567890";
        let nonce = "abc";
        let key = "mykey";
        let body = b"hello";
        let mut hasher = Sha256::new();
        hasher.update(ts.as_bytes());
        hasher.update(nonce.as_bytes());
        hasher.update(key.as_bytes());
        hasher.update(body);
        let expected = format!("{:x}", hasher.finalize());
        assert!(verify_signature(ts, nonce, key, body, &expected));
    }

    #[test]
    fn verify_signature_wrong() {
        assert!(!verify_signature("ts", "nonce", "key", b"body", "bad_sig"));
    }

    #[test]
    fn rate_limiter_allows_within_limit() {
        let rl = RateLimiter::new(60, 3);
        assert!(!rl.check("ip1"));
        assert!(!rl.check("ip1"));
        assert!(!rl.check("ip1"));
    }

    #[test]
    fn rate_limiter_rejects_over_limit() {
        let rl = RateLimiter::new(60, 2);
        assert!(!rl.check("ip1"));
        assert!(!rl.check("ip1"));
        assert!(rl.check("ip1")); // 3rd request exceeds limit of 2
    }

    // --- Name resolution tests ---

    #[tokio::test]
    async fn resolve_user_name_success_and_cache() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "tenant_access_token": "t-tok", "expire": 7200
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/open-apis/contact/v3/users/ou_user1"))
            .and(header("authorization", "Bearer t-tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": { "user": { "name": "Alice", "open_id": "ou_user1" } }
            })))
            .expect(1) // should only be called once (cached on second call)
            .mount(&server)
            .await;

        let config = test_config();
        let token_cache = Arc::new(FeishuTokenCache::with_base(&config, &server.uri()));
        let name_cache = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let client = reqwest::Client::new();

        let name = resolve_user_name("ou_user1", &name_cache, &token_cache, &client, &server.uri()).await;
        assert_eq!(name, "Alice");

        // Second call should use cache (expect(1) above ensures no second API call)
        let name2 = resolve_user_name("ou_user1", &name_cache, &token_cache, &client, &server.uri()).await;
        assert_eq!(name2, "Alice");
    }

    #[tokio::test]
    async fn resolve_user_name_api_error_falls_back_to_open_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "tenant_access_token": "t-tok", "expire": 7200
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/open-apis/contact/v3/users/ou_unknown"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 40003, "msg": "user not found"
            })))
            .mount(&server)
            .await;

        let config = test_config();
        let token_cache = Arc::new(FeishuTokenCache::with_base(&config, &server.uri()));
        let name_cache = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let client = reqwest::Client::new();

        let name = resolve_user_name("ou_unknown", &name_cache, &token_cache, &client, &server.uri()).await;
        assert_eq!(name, "ou_unknown");
    }

    // --- extract_mentions tests ---

    #[test]
    fn extract_mentions_replacen_only_first() {
        // If mention key appears in normal text too, only the first occurrence is removed
        let mentions = vec![FeishuMention {
            key: Some("@_user_1".into()),
            id: Some(FeishuMentionId { open_id: Some("ou_bot".into()) }),
            name: Some("Bot".into()),
        }];
        let env = make_envelope("group", "@_user_1 tell me about @_user_1 patterns", "ou_user1", Some(mentions));
        let cfg = test_config();
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg).unwrap();
        // Only first @_user_1 removed, second preserved
        assert!(evt.content.text.contains("@_user_1"));
    }

    // --- allowed_users filtering ---

    #[test]
    fn parse_allowed_users_blocks_unlisted() {
        let env = make_envelope("p2p", "hello", "ou_stranger", None);
        let mut cfg = test_config();
        cfg.allowed_users = vec!["ou_vip".into()];
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_allowed_users_permits_listed() {
        let env = make_envelope("p2p", "hello", "ou_vip", None);
        let mut cfg = test_config();
        cfg.allowed_users = vec!["ou_vip".into()];
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_some());
    }

    // --- allowed_groups filtering ---

    #[test]
    fn parse_allowed_groups_blocks_unlisted() {
        let mentions = vec![FeishuMention {
            key: Some("@_user_1".into()),
            id: Some(FeishuMentionId { open_id: Some("ou_bot".into()) }),
            name: Some("Bot".into()),
        }];
        let env = make_envelope("group", "@_user_1 hello", "ou_user1", Some(mentions));
        let mut cfg = test_config();
        cfg.allowed_groups = vec!["oc_other".into()]; // oc_chat1 not in list
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_none());
    }

    #[test]
    fn parse_allowed_groups_permits_listed() {
        let mentions = vec![FeishuMention {
            key: Some("@_user_1".into()),
            id: Some(FeishuMentionId { open_id: Some("ou_bot".into()) }),
            name: Some("Bot".into()),
        }];
        let env = make_envelope("group", "@_user_1 hello", "ou_user1", Some(mentions));
        let mut cfg = test_config();
        cfg.allowed_groups = vec!["oc_chat1".into()];
        assert!(parse_message_event(&env, Some("ou_bot"), &cfg).is_some());
    }

    // --- Token TTL from API response ---

    #[tokio::test]
    async fn token_uses_api_expire_field() {
        let server = MockServer::start().await;
        // Return a short expire (10s). With 300s margin, token should be
        // considered expired immediately on second call.
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-short",
                "expire": 10
            })))
            .expect(2) // called twice because 10s < 300s margin → always expired
            .mount(&server)
            .await;

        let config = test_config();
        let cache = FeishuTokenCache::with_base(&config, &server.uri());
        let client = reqwest::Client::new();

        let t1 = cache.get_token(&client).await.unwrap();
        assert_eq!(t1, "t-short");
        // Second call should refresh (expire=10 < margin=300)
        let t2 = cache.get_token(&client).await.unwrap();
        assert_eq!(t2, "t-short");
        // expect(2) verifies it was called twice
    }

    // --- constant_time_eq ---

    #[test]
    fn constant_time_eq_same() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_different() {
        assert!(!constant_time_eq("abc123", "abc124"));
    }

    #[test]
    fn constant_time_eq_different_length() {
        assert!(!constant_time_eq("short", "longer_string"));
    }

    // --- Thread ID parsing ---

    #[test]
    fn parse_thread_id_from_root_id() {
        let mut env = make_envelope("p2p", "reply", "ou_user1", None);
        env.event.as_mut().unwrap().message.as_mut().unwrap().root_id = Some("om_root".into());
        let cfg = test_config();
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg).unwrap();
        assert_eq!(evt.channel.thread_id, Some("om_root".into()));
    }

    #[test]
    fn parse_thread_id_from_parent_id() {
        let mut env = make_envelope("p2p", "reply", "ou_user1", None);
        env.event.as_mut().unwrap().message.as_mut().unwrap().parent_id = Some("om_parent".into());
        let cfg = test_config();
        let evt = parse_message_event(&env, Some("ou_bot"), &cfg).unwrap();
        assert_eq!(evt.channel.thread_id, Some("om_parent".into()));
    }

    // --- Emoji reaction mapping ---

    #[test]
    fn emoji_mapping_known() {
        assert_eq!(emoji_to_feishu_reaction("👍"), Some("THUMBSUP"));
        assert_eq!(emoji_to_feishu_reaction("🔥"), Some("FIRE"));
        assert_eq!(emoji_to_feishu_reaction("👀"), Some("EYES"));
    }

    #[test]
    fn emoji_mapping_unknown() {
        assert_eq!(emoji_to_feishu_reaction("🎉"), None);
    }
}

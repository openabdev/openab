mod adapters;
mod schema;

use anyhow::Result;
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use schema::GatewayReply;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

// --- Reply token cache for LINE hybrid Reply/Push dispatch ---

/// Cache entry for LINE reply tokens: (replyToken, insertion_time).
/// Uses std::sync::Mutex — critical sections are short (insert/remove/retain)
/// and never held across .await, so async Mutex overhead is unnecessary.
pub type ReplyTokenCache = Arc<std::sync::Mutex<HashMap<String, (String, Instant)>>>;

/// Maximum age (in seconds) before a cached reply token is considered expired.
/// LINE tokens are valid for ~1 minute; we use 50s as a conservative margin.
pub const REPLY_TOKEN_TTL_SECS: u64 = 50;

/// Maximum number of cached reply tokens. Prevents unbounded memory growth
/// if webhooks arrive faster than OAB can reply (e.g. OAB offline, spam burst).
pub const REPLY_TOKEN_CACHE_MAX: usize = 10_000;

// --- Media Store for Inbound Media Proxy (Issue #690) ---

/// uuid -> (binary data, mime type, created_at)
pub type MediaStore = Arc<std::sync::Mutex<HashMap<String, (Vec<u8>, String, Instant)>>>;

// --- App state (shared across all adapters) ---

pub struct AppState {
    /// Telegram bot token (None if Telegram disabled)
    pub telegram_bot_token: Option<String>,
    /// Telegram webhook secret token for request validation
    pub telegram_secret_token: Option<String>,
    /// LINE channel secret for signature validation
    pub line_channel_secret: Option<String>,
    /// LINE channel access token for reply API
    pub line_access_token: Option<String>,
    /// Teams adapter (None if Teams disabled)
    pub teams: Option<adapters::teams::TeamsAdapter>,
    /// service_url cache for Teams reply routing (conversation_id → (service_url, last_seen))
    pub teams_service_urls: Mutex<HashMap<String, (String, Instant)>>,
    /// Feishu adapter (None if Feishu disabled)
    pub feishu: Option<adapters::feishu::FeishuAdapter>,
    /// Google Chat adapter (None if Google Chat disabled)
    pub google_chat: Option<adapters::googlechat::GoogleChatAdapter>,
    /// WebSocket authentication token
    pub ws_token: Option<String>,
    /// Broadcast channel: gateway → OAB (events from all platforms)
    pub event_tx: broadcast::Sender<String>,
    /// Cache: event_id → (LINE replyToken, timestamp).
    pub reply_token_cache: ReplyTokenCache,
    /// Inbound media proxy store (UUID -> data)
    pub media_store: MediaStore,
    /// Public base URL for the gateway (used to construct attachment URLs)
    pub public_url: String,
    /// TTL for media in memory
    pub media_ttl: u64,
    /// Maximum number of entries in the media store
    pub media_max_entries: usize,
    /// Maximum file size for media downloads (in bytes, Issue #690 review fix)
    pub media_max_file_size: u64,
}

// --- WebSocket handler (OAB connects here) ---

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    query: axum::extract::Query<HashMap<String, String>>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(ref expected) = state.ws_token {
        let provided = query.get("token").map(|s| s.as_str());
        if provided != Some(expected.as_str()) {
            warn!("WebSocket rejected: invalid or missing token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_oab_connection(state, socket))
}

async fn handle_oab_connection(state: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();

    info!("OAB client connected via WebSocket");

    let last_event_id: Arc<Mutex<std::collections::HashMap<String, String>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Forward gateway events → OAB
    let last_event_id_for_send = last_event_id.clone();
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(event_json) = event_rx.recv() => {
                    // Track the last event ID sent to this client per channel
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&event_json) {
                        if let (Some(eid), Some(cid)) = (v["event_id"].as_str(), v["channel"]["id"].as_str()) {
                            let mut last = last_event_id_for_send.lock().await;
                            last.insert(cid.to_string(), eid.to_string());
                        }
                    }

                    if ws_tx.send(Message::Text(event_json.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Receive OAB replies → route to correct platform
    let state_for_recv = state.clone();
    // Track per-message reaction state (Telegram replaces all reactions atomically)
    let reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let last_event_id_for_recv = last_event_id.clone();
    let recv_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<GatewayReply>(&text) {
                    Ok(mut reply) => {
                        // Auto-fill reply_to if empty using the last event sent to this client.
                        // Rationale: This ensures agents don't need to track event IDs for simple replies.
                        // Note: reply_to is used by the LINE adapter to look up replyTokens; auto-filling
                        // allows LINE hybrid Reply/Push dispatch to work even if the agent omits reply_to.
                        // Issue #690 review fix: Only auto-fill if channel.id is non-empty to prevent
                        // cross-channel contamination (e.g., LINE reply tokens used for wrong messages).
                        if reply.reply_to.is_empty() && !reply.channel.id.is_empty() {
                            let last = last_event_id_for_recv.lock().await;
                            if let Some(eid) = last.get(&reply.channel.id) {
                                reply.reply_to = eid.clone();
                            }
                        }
                        info!(
                            platform = %reply.platform,
                            channel = %reply.channel.id,
                            command = ?reply.command.as_deref(),
                            "OAB → gateway reply"
                        );
                        match reply.platform.as_str() {
                            "telegram" => {
                                if let Some(ref token) = state_for_recv.telegram_bot_token {
                                    adapters::telegram::handle_reply(
                                        &reply,
                                        token,
                                        &client,
                                        &state_for_recv.event_tx,
                                        &reaction_state,
                                    )
                                    .await;
                                } else {
                                    warn!("reply for telegram but adapter not configured");
                                }
                            }
                            "line" => {
                                if let Some(ref access_token) = state_for_recv.line_access_token {
                                    adapters::line::dispatch_line_reply(
                                        &client,
                                        access_token,
                                        &state_for_recv.reply_token_cache,
                                        &reply,
                                        adapters::line::LINE_API_BASE,
                                    )
                                    .await;
                                } else {
                                    warn!("reply for line but adapter not configured");
                                }
                            }
                            "teams" => {
                                if let Some(ref teams) = state_for_recv.teams {
                                    adapters::teams::handle_reply(
                                        &reply,
                                        teams,
                                        &state_for_recv.teams_service_urls,
                                    )
                                    .await;
                                } else {
                                    warn!("reply for teams but adapter not configured");
                                }
                            }
                            "feishu" => {
                                if let Some(ref feishu) = state_for_recv.feishu {
                                    adapters::feishu::handle_reply(&reply, feishu, &state_for_recv.event_tx).await;
                                } else {
                                    warn!("reply for feishu but adapter not configured");
                                }
                            }
                            "googlechat" => {
                                if let Some(ref gc) = state_for_recv.google_chat {
                                    gc.handle_reply(&reply).await;
                                } else {
                                    warn!("reply for googlechat but adapter not configured");
                                }
                            }
                            other => warn!(platform = other, "unknown reply platform"),
                        }
                    }
                    Err(e) => warn!("invalid reply from OAB: {e}"),
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
    info!("OAB client disconnected");
}

async fn health() -> &'static str {
    "ok"
}

async fn media_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> impl IntoResponse {
    let entry = {
        let cache = state.media_store.lock().unwrap_or_else(|e| e.into_inner());
        cache.get(&uuid).cloned()
    };

    if let Some((data, mime, created_at)) = entry {
        // Check TTL before serving (Issue #690 review fix)
        if created_at.elapsed().as_secs() >= state.media_ttl {
            return axum::http::StatusCode::NOT_FOUND.into_response();
        }
        axum::response::Response::builder()
            .header("content-type", mime)
            .body(axum::body::Body::from(data))
            .unwrap()
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listen_addr = std::env::var("GATEWAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let ws_token = std::env::var("GATEWAY_WS_TOKEN").ok();
    
    // Inbound media proxy config (Issue #690)
    let public_url = std::env::var("GATEWAY_MEDIA_BASE_URL")
        .or_else(|_| std::env::var("GATEWAY_PUBLIC_URL"))
        .unwrap_or_else(|_| "http://localhost:8080".into());
    
    // Warn if public_url looks like a loopback address (Issue #690 review fix)
    if public_url.contains("localhost") || public_url.contains("127.0.0.1") {
        warn!(
            public_url = %public_url,
            "GATEWAY_PUBLIC_URL looks like a loopback address. Media attachments will not be reachable from other pods/containers. Set GATEWAY_PUBLIC_URL to the gateway's cluster-internal Service URL (e.g. http://openab-gateway:8080) or its external ingress URL."
        );
    }
    
    let media_ttl = std::env::var("GATEWAY_MEDIA_STORE_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);
    let media_max_entries = std::env::var("GATEWAY_MEDIA_STORE_MAX_ENTRIES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000);
    // Issue #690 review fix: per-file size limit to prevent OOM
    let media_max_file_size = std::env::var("GATEWAY_MEDIA_MAX_FILE_SIZE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10 * 1024 * 1024); // 10MB default

    if ws_token.is_none() {
        warn!("GATEWAY_WS_TOKEN not set — WebSocket connections are NOT authenticated (insecure)");
    }

    let (event_tx, _) = broadcast::channel::<String>(256);
    let reply_token_cache: ReplyTokenCache = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let media_store: MediaStore = Arc::new(std::sync::Mutex::new(HashMap::new()));

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/media/{uuid}", get(media_handler));

    // Telegram adapter
    let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let telegram_secret_token = std::env::var("TELEGRAM_SECRET_TOKEN").ok();
    if telegram_bot_token.is_some() {
        let webhook_path =
            std::env::var("TELEGRAM_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/telegram".into());
        if telegram_secret_token.is_none() {
            warn!("TELEGRAM_SECRET_TOKEN not set — webhook requests are NOT validated (insecure)");
        }
        info!(path = %webhook_path, "telegram adapter enabled");
        app = app.route(&webhook_path, post(adapters::telegram::webhook));
    }

    // LINE adapter — route is always mounted so inbound webhooks are accepted
    // even without an access token (signature validation only needs LINE_CHANNEL_SECRET).
    let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok();
    let line_access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();
    info!("line adapter enabled");
    app = app.route("/webhook/line", post(adapters::line::webhook));

    // Teams adapter
    let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
        info!("teams adapter enabled");
        adapters::teams::TeamsAdapter::new(config)
    });
    if teams.is_some() {
        let webhook_path =
            std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());
        info!(path = %webhook_path, "teams webhook registered");
        app = app.route(&webhook_path, post(adapters::teams::webhook));
    }

    // Feishu adapter
    let feishu_config = adapters::feishu::FeishuConfig::from_env();
    let feishu_ws_mode = feishu_config
        .as_ref()
        .map(|c| c.connection_mode == adapters::feishu::ConnectionMode::Websocket)
        .unwrap_or(false);
    if let Some(ref config) = feishu_config {
        match config.connection_mode {
            adapters::feishu::ConnectionMode::Websocket => {
                info!("feishu adapter enabled (websocket) — will connect after state init");
            }
            adapters::feishu::ConnectionMode::Webhook => {
                let path = config.webhook_path.clone();
                info!(path = %path, "feishu adapter enabled (webhook)");
                app = app.route(&path, post(adapters::feishu::webhook));
            }
        }
    }
    let feishu = feishu_config.map(adapters::feishu::FeishuAdapter::new);

    // Resolve feishu bot identity early (needed for mention gating in both modes)
    if let Some(ref f) = feishu {
        f.resolve_bot_identity().await;
    }

    // Google Chat adapter
    let google_chat_enabled = std::env::var("GOOGLE_CHAT_ENABLED")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let google_chat = if google_chat_enabled {
        let token_cache = std::env::var("GOOGLE_CHAT_SA_KEY_JSON")
            .ok()
            .or_else(|| {
                std::env::var("GOOGLE_CHAT_SA_KEY_FILE")
                    .ok()
                    .and_then(|path| std::fs::read_to_string(&path).ok())
            })
            .and_then(|json| {
                adapters::googlechat::GoogleChatTokenCache::new(&json)
                    .map_err(|e| warn!("googlechat SA key error: {e}"))
                    .ok()
            });
        let access_token = std::env::var("GOOGLE_CHAT_ACCESS_TOKEN").ok();
        let jwt_verifier = std::env::var("GOOGLE_CHAT_AUDIENCE").ok().map(|aud| {
            info!("googlechat webhook JWT verification enabled (audience={aud})");
            adapters::googlechat::GoogleChatJwtVerifier::new(aud)
        });

        let webhook_path = std::env::var("GOOGLE_CHAT_WEBHOOK_PATH")
            .unwrap_or_else(|_| "/webhook/googlechat".into());
        info!(path = %webhook_path, "googlechat adapter enabled");
        app = app.route(&webhook_path, post(adapters::googlechat::webhook));

        if token_cache.is_some() {
            info!("googlechat service account configured — token auto-refresh enabled");
        } else if access_token.is_some() {
            warn!("googlechat using static access token — will expire in ~1 hour");
        } else {
            warn!("GOOGLE_CHAT_ACCESS_TOKEN / GOOGLE_CHAT_SA_KEY_JSON not set — replies will be logged but not sent");
        }
        if jwt_verifier.is_none() {
            warn!("GOOGLE_CHAT_AUDIENCE not set — webhook requests are NOT authenticated (insecure)");
        }

        Some(adapters::googlechat::GoogleChatAdapter::new(token_cache, access_token, jwt_verifier))
    } else {
        None
    };

    if telegram_bot_token.is_none()
        && line_access_token.is_none()
        && teams.is_none()
        && feishu.is_none()
        && google_chat.is_none()
    {
        warn!("no adapters configured — set TELEGRAM_BOT_TOKEN, LINE_CHANNEL_ACCESS_TOKEN, TEAMS_APP_ID + TEAMS_APP_SECRET, FEISHU_APP_ID + FEISHU_APP_SECRET, and/or GOOGLE_CHAT_ENABLED=true");
    }

    let state = Arc::new(AppState {
        telegram_bot_token,
        telegram_secret_token,
        line_channel_secret,
        line_access_token,
        teams,
        teams_service_urls: Mutex::new(HashMap::new()),
        feishu,
        google_chat,
        ws_token,
        event_tx,
        reply_token_cache,
        media_store,
        public_url,
        media_ttl,
        media_max_entries,
        media_max_file_size,
    });

    // Background task: sweep expired reply tokens every REPLY_TOKEN_TTL_SECS
    {
        let cache_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(REPLY_TOKEN_TTL_SECS)).await;
                let mut cache = cache_state
                    .reply_token_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let before = cache.len();
                cache.retain(|_, (_, t)| t.elapsed().as_secs() < REPLY_TOKEN_TTL_SECS);
                let after = cache.len();
                if before != after {
                    info!(
                        removed = before - after,
                        remaining = after,
                        "reply token cache sweep"
                    );
                }
            }
        });
    }

    // Background task: sweep expired media every 60 seconds (Issue #690)
    {
        let store = state.media_store.clone();
        let ttl = state.media_ttl;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let mut cache = store.lock().unwrap_or_else(|e| e.into_inner());
                let before = cache.len();
                cache.retain(|_, (_, _, t)| t.elapsed().as_secs() < ttl);
                let after = cache.len();
                if before != after {
                    info!(
                        removed = before - after,
                        remaining = after,
                        "media store sweep"
                    );
                }
            }
        });
    }

    // Periodic cleanup of stale Teams service_url entries (TTL: 4 hours)
    {
        let state_for_cleanup = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let mut urls = state_for_cleanup.teams_service_urls.lock().await;
                let before = urls.len();
                urls.retain(|_, (_, t)| t.elapsed().as_secs() < 4 * 3600);
                let after = urls.len();
                if before != after {
                    info!(
                        removed = before - after,
                        remaining = after,
                        "teams service_url cache cleanup"
                    );
                }
            }
        });
    }

    let app = app.with_state(state.clone());

    // Spawn feishu WebSocket long-connection if configured
    // feishu_shutdown_tx must remain alive for the lifetime of main() — dropping
    // it signals shutdown to the WS task via feishu_shutdown_rx.
    let (feishu_shutdown_tx, feishu_shutdown_rx) = tokio::sync::watch::channel(false);
    if feishu_ws_mode {
        if let Some(ref feishu) = state.feishu {
            match adapters::feishu::start_websocket(feishu, state.event_tx.clone(), feishu_shutdown_rx).await {
                Ok(_handle) => info!("feishu websocket task spawned"),
                Err(e) => tracing::error!(err = %e, "feishu websocket startup failed"),
            }
        }
    }

    info!(addr = %listen_addr, "gateway starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    drop(feishu_shutdown_tx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_reply(event_id: &str) -> schema::GatewayReply {
        schema::GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: event_id.into(),
            platform: "line".into(),
            channel: schema::ReplyChannel {
                id: "U1234".into(),
                thread_id: None,
            },
            content: schema::Content {
                content_type: "text".into(),
                text: "hello".into(),
                attachments: Vec::new(),
            },
            command: None,
            request_id: None,
        }
    }

    fn make_cache() -> ReplyTokenCache {
        Arc::new(std::sync::Mutex::new(HashMap::new()))
    }

    /// Cache hit: uses Reply API with correct replyToken, bearer token, and message body.
    /// Does NOT call Push API.
    #[tokio::test]
    async fn cache_hit_uses_reply_api() {
        let server = MockServer::start().await;
        let _reply = Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .and(header("authorization", "Bearer test_access_token"))
            .and(body_json(serde_json::json!({
                "replyToken": "tok_abc",
                "messages": [{"type": "text", "text": "hello"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let cache = make_cache();
        cache
            .lock()
            .unwrap()
            .insert("evt_1".into(), ("tok_abc".into(), Instant::now()));

        let client = reqwest::Client::new();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_1"),
            &server.uri(),
        )
        .await;

        assert!(used, "should report Reply API was used");
    }

    /// Cache miss: falls back to Push API with correct "to", bearer token, and message body.
    #[tokio::test]
    async fn cache_miss_uses_push_api() {
        let server = MockServer::start().await;
        let _reply = Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .and(header("authorization", "Bearer test_access_token"))
            .and(body_json(serde_json::json!({
                "to": "U1234",
                "messages": [{"type": "text", "text": "hello"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let cache = make_cache();

        let client = reqwest::Client::new();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_miss"),
            &server.uri(),
        )
        .await;

        assert!(!used, "should report Push API was used (no reply token)");
    }

    /// Expired cached token: falls back to Push API.
    #[tokio::test]
    async fn expired_token_uses_push_api() {
        let server = MockServer::start().await;
        let _reply = Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .and(header("authorization", "Bearer test_access_token"))
            .and(body_json(serde_json::json!({
                "to": "U1234",
                "messages": [{"type": "text", "text": "hello"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let cache = make_cache();
        let expired_time = Instant::now() - Duration::from_secs(REPLY_TOKEN_TTL_SECS + 10);
        cache
            .lock()
            .unwrap()
            .insert("evt_exp".into(), ("tok_old".into(), expired_time));

        let client = reqwest::Client::new();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_exp"),
            &server.uri(),
        )
        .await;

        assert!(!used, "should report Push API was used (expired token)");
    }

    /// Reply API 400 with invalid/expired reply token: falls back to Push API.
    #[tokio::test]
    async fn reply_400_invalid_token_falls_back_to_push() {
        let server = MockServer::start().await;
        let _reply = Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .and(header("authorization", "Bearer test_access_token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"message":"Invalid reply token"}"#),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .and(header("authorization", "Bearer test_access_token"))
            .and(body_json(serde_json::json!({
                "to": "U1234",
                "messages": [{"type": "text", "text": "hello"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let cache = make_cache();
        cache
            .lock()
            .unwrap()
            .insert("evt_400".into(), ("tok_bad".into(), Instant::now()));

        let client = reqwest::Client::new();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_400"),
            &server.uri(),
        )
        .await;

        assert!(!used, "should fall back to Push on 400 invalid token");
    }

    /// Reply API 5xx: does NOT fall back to Push (duplicate risk).
    #[tokio::test]
    async fn reply_5xx_does_not_fallback() {
        let server = MockServer::start().await;
        let _reply = Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .and(header("authorization", "Bearer test_access_token"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let _push = Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let cache = make_cache();
        cache
            .lock()
            .unwrap()
            .insert("evt_5xx".into(), ("tok_5xx".into(), Instant::now()));

        let client = reqwest::Client::new();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_5xx"),
            &server.uri(),
        )
        .await;

        assert!(used, "should NOT fall back to Push on 5xx");
    }

    /// Reply API network/timeout error: does NOT fall back to Push (duplicate risk).
    #[tokio::test]
    async fn reply_network_error_does_not_fallback() {
        let bad_base = "http://127.0.0.1:1";

        let cache = make_cache();
        cache
            .lock()
            .unwrap()
            .insert("evt_net".into(), ("tok_net".into(), Instant::now()));

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let used = adapters::line::dispatch_line_reply(
            &client,
            "test_access_token",
            &cache,
            &make_reply("evt_net"),
            bad_base,
        )
        .await;

        assert!(used, "should NOT fall back to Push on network error");
    }
}

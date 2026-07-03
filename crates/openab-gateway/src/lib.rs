pub mod adapters;
pub(crate) mod media;
pub mod schema;
pub mod store;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Mutex, Semaphore};

// --- Reply token cache for LINE hybrid Reply/Push dispatch ---

/// Cache entry for LINE reply tokens: (replyToken, insertion_time).
pub type ReplyTokenCache = Arc<std::sync::Mutex<HashMap<String, (String, Instant)>>>;

/// Maximum age (in seconds) before a cached reply token is considered expired.
pub const REPLY_TOKEN_TTL_SECS: u64 = 50;

/// Maximum number of cached reply tokens.
pub const REPLY_TOKEN_CACHE_MAX: usize = 10_000;

/// Maximum number of post-ack LINE webhook payloads processed concurrently.
pub const LINE_WEBHOOK_CONCURRENCY_MAX: usize = 8;

// --- App state (shared across all adapters) ---

pub struct AppState {
    pub telegram_bot_token: Option<String>,
    pub telegram_secret_token: Option<String>,
    pub telegram_rich_messages: bool,
    pub telegram_trusted_source_only: bool,
    /// Streaming override. `None` = follow `telegram_rich_messages`.
    pub telegram_streaming: Option<bool>,
    pub line_channel_secret: Option<String>,
    pub line_access_token: Option<String>,
    #[cfg(feature = "teams")]
    pub teams: Option<adapters::teams::TeamsAdapter>,
    pub teams_service_urls: Mutex<HashMap<String, (String, Instant)>>,
    #[cfg(feature = "feishu")]
    pub feishu: Option<adapters::feishu::FeishuAdapter>,
    #[cfg(feature = "googlechat")]
    pub google_chat: Option<adapters::googlechat::GoogleChatAdapter>,
    #[cfg(feature = "wecom")]
    pub wecom: Option<adapters::wecom::WecomAdapter>,
    pub ws_token: Option<String>,
    pub event_tx: broadcast::Sender<String>,
    pub reply_token_cache: ReplyTokenCache,
    pub line_webhook_semaphore: Arc<Semaphore>,
    pub client: reqwest::Client,
}


impl AppState {
    /// Create a minimal AppState for testing. Only requires an `event_tx` sender;
    /// all adapter fields default to `None`/empty. This decouples adapter tests
    /// from each other — adding a new adapter no longer forces changes in
    /// unrelated test files.
    ///
    /// NOTE: Interim fix — the long-term solution is a full AdapterRegistry
    /// (trait-object pattern) per the remaining scope of #1239.
    ///
    /// See: <https://github.com/openabdev/openab/issues/1239>
    pub fn test_default(event_tx: broadcast::Sender<String>) -> Self {
        Self {
            telegram_bot_token: None,
            telegram_secret_token: None,
            telegram_rich_messages: false,
            telegram_trusted_source_only: false,
            telegram_streaming: None,
            line_channel_secret: None,
            line_access_token: None,
            #[cfg(feature = "teams")]
            teams: None,
            teams_service_urls: Mutex::new(HashMap::new()),
            #[cfg(feature = "feishu")]
            feishu: None,
            #[cfg(feature = "googlechat")]
            google_chat: None,
            #[cfg(feature = "wecom")]
            wecom: None,
            ws_token: None,
            event_tx,
            reply_token_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
            client: reqwest::Client::new(),
        }
    }

    /// Build AppState from environment variables.
    /// Initializes all platform adapters based on available env vars.
    /// `ws_token` is passed separately (only needed for standalone gateway mode).
    pub fn from_env(event_tx: broadcast::Sender<String>, ws_token: Option<String>) -> Self {
        use tracing::{info, warn};

        // Telegram
        let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
        let telegram_secret_token = std::env::var("TELEGRAM_SECRET_TOKEN").ok();
        let telegram_rich_messages = std::env::var("TELEGRAM_RICH_MESSAGES")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let telegram_trusted_source_only = std::env::var("TELEGRAM_TRUSTED_SOURCE_ONLY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let telegram_streaming = std::env::var("TELEGRAM_STREAMING")
            .ok()
            .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")));

        // LINE
        let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok();
        let line_access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();

        // Teams
        #[cfg(feature = "teams")]
        let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
            info!("teams adapter configured");
            adapters::teams::TeamsAdapter::new(config)
        });

        // Feishu
        #[cfg(feature = "feishu")]
        let feishu = adapters::feishu::FeishuConfig::from_env()
            .map(adapters::feishu::FeishuAdapter::new);

        // Google Chat
        #[cfg(feature = "googlechat")]
        let google_chat = {
            let enabled = std::env::var("GOOGLE_CHAT_ENABLED")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            if enabled {
                let token_cache = std::env::var("GOOGLE_CHAT_SA_KEY_JSON")
                    .ok()
                    .or_else(|| {
                        std::env::var("GOOGLE_CHAT_SA_KEY_FILE")
                            .ok()
                            .and_then(|path| {
                                std::fs::read_to_string(&path).map_err(|e| {
                                    warn!("failed to read GOOGLE_CHAT_SA_KEY_FILE '{}': {e}", path);
                                }).ok()
                            })
                    })
                    .and_then(|json| {
                        adapters::googlechat::GoogleChatTokenCache::new(&json)
                            .map_err(|e| warn!("googlechat SA key error: {e}"))
                            .ok()
                    });
                let access_token = std::env::var("GOOGLE_CHAT_ACCESS_TOKEN").ok();
                let jwt_verifier = std::env::var("GOOGLE_CHAT_AUDIENCE").ok().map(|aud| {
                    info!("googlechat JWT verification enabled (audience={aud})");
                    adapters::googlechat::GoogleChatJwtVerifier::new(aud)
                });
                Some(adapters::googlechat::GoogleChatAdapter::new(
                    token_cache, access_token, jwt_verifier,
                ))
            } else {
                None
            }
        };

        // WeCom
        #[cfg(feature = "wecom")]
        let wecom = adapters::wecom::WecomConfig::from_env()
            .map(adapters::wecom::WecomAdapter::new);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("HTTP client must build");

        Self {
            telegram_bot_token,
            telegram_secret_token,
            telegram_rich_messages,
            telegram_trusted_source_only,
            telegram_streaming,
            line_channel_secret,
            line_access_token,
            #[cfg(feature = "teams")]
            teams,
            teams_service_urls: Mutex::new(HashMap::new()),
            #[cfg(feature = "feishu")]
            feishu,
            #[cfg(feature = "googlechat")]
            google_chat,
            #[cfg(feature = "wecom")]
            wecom,
            ws_token,
            event_tx,
            reply_token_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
            client,
        }
    }

    /// Apply resolved `[telegram]` config values, overriding the env-derived
    /// fields. Accepts a `GatewayTelegramConfig` to keep this crate free of an
    /// `openab-core` dependency (the binary crate resolves config → this struct).
    pub fn apply_telegram_config(&mut self, cfg: GatewayTelegramConfig) {
        self.telegram_bot_token = cfg.bot_token;
        self.telegram_secret_token = cfg.secret_token;
        self.telegram_rich_messages = cfg.rich_messages;
        self.telegram_trusted_source_only = cfg.trusted_source_only;
        self.telegram_streaming = cfg.streaming;
    }
}

/// Parameter object for passing resolved Telegram config across the crate
/// boundary without introducing a dependency on `openab-core`.
#[derive(Debug, Clone)]
pub struct GatewayTelegramConfig {
    pub bot_token: Option<String>,
    pub secret_token: Option<String>,
    pub rich_messages: bool,
    pub trusted_source_only: bool,
    pub streaming: Option<bool>,
}

// --- Public serve() entry point ---

/// Configuration for the standalone gateway server.
pub struct ServeConfig {
    pub listen_addr: String,
    pub ws_token: Option<String>,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            listen_addr: std::env::var("GATEWAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            ws_token: std::env::var("GATEWAY_WS_TOKEN").ok(),
        }
    }
}

/// Start the standalone gateway server. This is the main entry point extracted
/// from the gateway binary — the binary becomes a thin wrapper around this.
pub async fn serve(config: ServeConfig) -> anyhow::Result<()> {
    use axum::{routing::{get, post}, Router};
    use tracing::{info, warn};

    let ServeConfig { listen_addr, ws_token } = config;

    if ws_token.is_none() {
        warn!("GATEWAY_WS_TOKEN not set — WebSocket connections are NOT authenticated (insecure)");
    }

    let (event_tx, _) = broadcast::channel::<String>(256);
    let reply_token_cache: ReplyTokenCache = Arc::new(std::sync::Mutex::new(HashMap::new()));

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health));

    // Telegram adapter
    #[cfg(feature = "telegram")]
    let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    #[cfg(feature = "telegram")]
    let telegram_secret_token = std::env::var("TELEGRAM_SECRET_TOKEN").ok();
    #[cfg(feature = "telegram")]
    let telegram_rich_messages = std::env::var("TELEGRAM_RICH_MESSAGES")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    #[cfg(feature = "telegram")]
    if telegram_bot_token.is_some() {
        let webhook_path =
            std::env::var("TELEGRAM_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/telegram".into());
        if telegram_secret_token.is_none() {
            warn!("TELEGRAM_SECRET_TOKEN not set — webhook requests are NOT validated (insecure)");
        }
        info!(path = %webhook_path, "telegram adapter enabled");
        app = app.route(&webhook_path, post(adapters::telegram::webhook));
    }
    #[cfg(not(feature = "telegram"))]
    let telegram_bot_token: Option<String> = None;
    #[cfg(not(feature = "telegram"))]
    let telegram_secret_token: Option<String> = None;
    #[cfg(not(feature = "telegram"))]
    let telegram_rich_messages = false;

    // LINE adapter
    #[cfg(feature = "line")]
    let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok();
    #[cfg(feature = "line")]
    let line_access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();
    #[cfg(feature = "line")]
    {
        info!("line adapter enabled");
        app = app.route("/webhook/line", post(adapters::line::webhook));
    }
    #[cfg(not(feature = "line"))]
    let line_channel_secret: Option<String> = None;
    #[cfg(not(feature = "line"))]
    let line_access_token: Option<String> = None;

    // Teams adapter
    #[cfg(feature = "teams")]
    let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
        info!("teams adapter enabled");
        adapters::teams::TeamsAdapter::new(config)
    });
    #[cfg(not(feature = "teams"))]
    let teams: Option<()> = None;

    #[cfg(feature = "teams")]
    if teams.is_some() {
        let webhook_path =
            std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());
        info!(path = %webhook_path, "teams webhook registered");
        app = app.route(&webhook_path, post(adapters::teams::webhook));
    }

    // Feishu adapter
    #[cfg(feature = "feishu")]
    let feishu_config = adapters::feishu::FeishuConfig::from_env();
    #[cfg(feature = "feishu")]
    let feishu_ws_mode = feishu_config
        .as_ref()
        .map(|c| c.connection_mode == adapters::feishu::ConnectionMode::Websocket)
        .unwrap_or(false);
    #[cfg(feature = "feishu")]
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
    #[cfg(feature = "feishu")]
    let feishu = feishu_config.map(adapters::feishu::FeishuAdapter::new);
    #[cfg(not(feature = "feishu"))]
    let feishu: Option<()> = None;
    #[cfg(not(feature = "feishu"))]
    let feishu_ws_mode = false;

    // Resolve feishu bot identity early
    #[cfg(feature = "feishu")]
    if let Some(ref f) = feishu {
        f.resolve_bot_identity().await;
        if f.config.streaming_mode != adapters::feishu::StreamingMode::Post {
            let sessions = f.stream_sessions.clone();
            let token_cache = f.token_cache.clone();
            let client = f.client.clone();
            let api_base = f.config.api_base();
            let idle_ms = f.config.card_idle_finalize_ms;
            tokio::spawn(adapters::feishu::run_idle_reaper(
                sessions, token_cache, client, api_base, idle_ms,
            ));
            info!(idle_ms, "feishu card-streaming idle reaper started");
        }
    }

    // Google Chat adapter
    #[cfg(feature = "googlechat")]
    let google_chat = {
        let enabled = std::env::var("GOOGLE_CHAT_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if enabled {
            let token_cache = std::env::var("GOOGLE_CHAT_SA_KEY_JSON")
                .ok()
                .or_else(|| {
                    std::env::var("GOOGLE_CHAT_SA_KEY_FILE")
                        .ok()
                        .and_then(|path| {
                            std::fs::read_to_string(&path).map_err(|e| {
                                warn!("failed to read GOOGLE_CHAT_SA_KEY_FILE '{}': {e}", path);
                            }).ok()
                        })
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

            Some(adapters::googlechat::GoogleChatAdapter::new(
                token_cache,
                access_token,
                jwt_verifier,
            ))
        } else {
            None
        }
    };
    #[cfg(not(feature = "googlechat"))]
    let google_chat: Option<()> = None;

    // WeCom adapter
    #[cfg(feature = "wecom")]
    let wecom = adapters::wecom::WecomConfig::from_env().map(|config| {
        let path = config.webhook_path.clone();
        info!(path = %path, "wecom adapter enabled");
        adapters::wecom::WecomAdapter::new(config)
    });
    #[cfg(feature = "wecom")]
    if let Some(ref w) = wecom {
        app = app
            .route(
                &w.config.webhook_path,
                axum::routing::get(adapters::wecom::verify),
            )
            .route(&w.config.webhook_path, post(adapters::wecom::webhook));
    }
    #[cfg(not(feature = "wecom"))]
    let wecom: Option<()> = None;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("HTTP client must build");

    let state = Arc::new(AppState {
        telegram_bot_token,
        telegram_secret_token,
        telegram_rich_messages,
        telegram_trusted_source_only: std::env::var("TELEGRAM_TRUSTED_SOURCE_ONLY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        telegram_streaming: std::env::var("TELEGRAM_STREAMING")
            .ok()
            .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false"))),
        line_channel_secret,
        line_access_token,
        #[cfg(feature = "teams")]
        teams,
        teams_service_urls: Mutex::new(HashMap::new()),
        #[cfg(feature = "feishu")]
        feishu,
        #[cfg(feature = "googlechat")]
        google_chat,
        #[cfg(feature = "wecom")]
        wecom,
        ws_token,
        event_tx,
        reply_token_cache,
        line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
        client,
    });

    // Background: sweep expired reply tokens
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
                    info!(removed = before - after, remaining = after, "reply token cache sweep");
                }
            }
        });
    }

    // Background: cleanup stale Teams service_url entries (TTL: 4h)
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
                    info!(removed = before - after, remaining = after, "teams service_url cache cleanup");
                }
            }
        });
    }

    let app = app.with_state(state.clone());

    // Background: evict expired media files
    tokio::spawn(store::eviction_loop());

    // Spawn feishu WebSocket long-connection if configured
    let (feishu_shutdown_tx, feishu_shutdown_rx) = tokio::sync::watch::channel(false);
    #[cfg(feature = "feishu")]
    if feishu_ws_mode {
        if let Some(ref feishu) = state.feishu {
            match adapters::feishu::start_websocket(
                feishu,
                state.event_tx.clone(),
                feishu_shutdown_rx,
            )
            .await
            {
                Ok(_handle) => info!("feishu websocket task spawned"),
                Err(e) => tracing::error!(err = %e, "feishu websocket startup failed"),
            }
        }
    }
    #[cfg(not(feature = "feishu"))]
    let _ = feishu_shutdown_rx;

    info!(addr = %listen_addr, "gateway starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    drop(feishu_shutdown_tx);
    Ok(())
}

// --- Internal handler functions used by serve() ---

async fn ws_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    query: axum::extract::Query<HashMap<String, String>>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use tracing::warn;

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
    use futures_util::{SinkExt, StreamExt};
    use tracing::{info, warn};

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();

    info!("OAB client connected via WebSocket");

    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(event_json) = event_rx.recv() => {
                    if ws_tx.send(Message::Text(event_json.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let state_for_recv = state.clone();
    let reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let recv_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<schema::GatewayReply>(&text) {
                    Ok(reply) => {
                        info!(
                            platform = %reply.platform,
                            channel = %reply.channel.id,
                            command = ?reply.command.as_deref(),
                            "OAB → gateway reply"
                        );
                        match reply.platform.as_str() {
                            #[cfg(feature = "telegram")]
                            "telegram" => {
                                if let Some(ref token) = state_for_recv.telegram_bot_token {
                                    adapters::telegram::handle_reply(
                                        &reply,
                                        token,
                                        &client,
                                        &state_for_recv.event_tx,
                                        &reaction_state,
                                        state_for_recv.telegram_rich_messages,
                                    )
                                    .await;
                                } else {
                                    warn!("reply for telegram but adapter not configured");
                                }
                            }
                            #[cfg(feature = "line")]
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
                            #[cfg(feature = "teams")]
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
                            #[cfg(feature = "feishu")]
                            "feishu" => {
                                if let Some(ref feishu) = state_for_recv.feishu {
                                    adapters::feishu::handle_reply(
                                        &reply,
                                        feishu,
                                        &state_for_recv.event_tx,
                                    )
                                    .await;
                                } else {
                                    warn!("reply for feishu but adapter not configured");
                                }
                            }
                            #[cfg(feature = "googlechat")]
                            "googlechat" => {
                                if let Some(ref gc) = state_for_recv.google_chat {
                                    gc.handle_reply(&reply, &state_for_recv.event_tx).await;
                                } else {
                                    warn!("reply for googlechat but adapter not configured");
                                }
                            }
                            #[cfg(feature = "wecom")]
                            "wecom" => {
                                if let Some(ref wecom) = state_for_recv.wecom {
                                    wecom.handle_reply(&reply, &state_for_recv.event_tx).await;
                                } else {
                                    warn!("reply for wecom but adapter not configured");
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

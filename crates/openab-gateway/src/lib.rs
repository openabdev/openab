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

/// Whether a webhook platform's L1 (transport authentication) is unenforceable:
/// the platform is active (configured to receive traffic) but its verification
/// secret is not configured, so it accepts unauthenticated POSTs. See #1356.
fn l1_unenforceable(active: bool, l1_configured: bool) -> bool {
    active && !l1_configured
}

pub struct AppState {
    pub telegram_bot_token: Option<String>,
    pub telegram_secret_token: Option<String>,
    pub telegram_rich_messages: bool,
    pub telegram_trusted_source_only: bool,
    /// Streaming override. `None` = follow `telegram_rich_messages`.
    pub telegram_streaming: Option<bool>,
    pub line_channel_secret: Option<String>,
    pub line_access_token: Option<String>,
    /// Webhook mount path for LINE (env: `LINE_WEBHOOK_PATH`; config-first via
    /// `apply_line_config`, default `/webhook/line`).
    pub line_webhook_path: String,
    #[cfg(feature = "teams")]
    pub teams: Option<adapters::teams::TeamsAdapter>,
    /// Webhook mount path for Teams (env: `TEAMS_WEBHOOK_PATH`; config-first
    /// via `apply_teams_config`, default `/webhook/teams`).
    pub teams_webhook_path: String,
    pub teams_service_urls: Mutex<HashMap<String, (String, Instant)>>,
    #[cfg(feature = "feishu")]
    pub feishu: Option<adapters::feishu::FeishuAdapter>,
    #[cfg(feature = "googlechat")]
    pub google_chat: Option<adapters::googlechat::GoogleChatAdapter>,
    /// Webhook mount path for Google Chat (env: `GOOGLE_CHAT_WEBHOOK_PATH`;
    /// config-first via `apply_googlechat_config`, default `/webhook/googlechat`).
    pub googlechat_webhook_path: String,
    #[cfg(feature = "wecom")]
    pub wecom: Option<adapters::wecom::WecomAdapter>,
    #[cfg(feature = "acp")]
    pub acp: Option<adapters::acp_server::AcpConfig>,
    #[cfg(feature = "acp")]
    pub acp_reply_registry: Option<adapters::acp_server::AcpReplyRegistry>,
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
            line_webhook_path: "/webhook/line".into(),
            #[cfg(feature = "teams")]
            teams: None,
            teams_webhook_path: "/webhook/teams".into(),
            teams_service_urls: Mutex::new(HashMap::new()),
            #[cfg(feature = "feishu")]
            feishu: None,
            #[cfg(feature = "googlechat")]
            google_chat: None,
            googlechat_webhook_path: "/webhook/googlechat".into(),
            #[cfg(feature = "wecom")]
            wecom: None,
            #[cfg(feature = "acp")]
            acp: None,
            #[cfg(feature = "acp")]
            acp_reply_registry: None,
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
        use tracing::info;

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
        let line_webhook_path =
            std::env::var("LINE_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/line".into());

        // Teams
        #[cfg(feature = "teams")]
        let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
            info!("teams adapter configured");
            adapters::teams::TeamsAdapter::new(config)
        });
        let teams_webhook_path =
            std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());

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
                Some(adapters::googlechat::GoogleChatAdapter::from_parts(
                    std::env::var("GOOGLE_CHAT_SA_KEY_JSON").ok(),
                    std::env::var("GOOGLE_CHAT_SA_KEY_FILE").ok(),
                    std::env::var("GOOGLE_CHAT_ACCESS_TOKEN").ok(),
                    std::env::var("GOOGLE_CHAT_AUDIENCE").ok(),
                ))
            } else {
                None
            }
        };
        let googlechat_webhook_path = std::env::var("GOOGLE_CHAT_WEBHOOK_PATH")
            .unwrap_or_else(|_| "/webhook/googlechat".into());

        // WeCom
        #[cfg(feature = "wecom")]
        let wecom = adapters::wecom::WecomConfig::from_env()
            .map(adapters::wecom::WecomAdapter::new);

        // ACP Server
        #[cfg(feature = "acp")]
        let acp = adapters::acp_server::AcpConfig::from_env();
        #[cfg(feature = "acp")]
        let acp_reply_registry = acp.as_ref().map(|_| adapters::acp_server::new_reply_registry());

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
            line_webhook_path,
            #[cfg(feature = "teams")]
            teams,
            teams_webhook_path,
            teams_service_urls: Mutex::new(HashMap::new()),
            #[cfg(feature = "feishu")]
            feishu,
            #[cfg(feature = "googlechat")]
            google_chat,
            googlechat_webhook_path,
            #[cfg(feature = "wecom")]
            wecom,
            #[cfg(feature = "acp")]
            acp,
            #[cfg(feature = "acp")]
            acp_reply_registry,
            ws_token,
            event_tx,
            reply_token_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
            client,
        }
    }

    /// Phase 1 L1 audit (#1356): warn loudly for each **active** webhook
    /// platform whose transport authentication (L1) secret is unconfigured.
    ///
    /// When L1 is skipped, the webhook accepts unauthenticated POSTs, so the
    /// per-platform `allowed_users` (L3) allowlist is forgeable — an attacker
    /// can POST an envelope with an allowlisted sender id and pass the trust
    /// gate. Phase 1 only warns (backward-compatible: existing no-secret
    /// deployments keep running); a later phase may escalate to a hard error.
    ///
    /// `feishu_webhook_route_mounted`: whether the caller actually mounted the
    /// Feishu webhook route. The two binaries differ — the standalone gateway
    /// mounts it only in Webhook connection mode, while the unified binary
    /// mounts it unconditionally — so exposure is the caller's knowledge, not
    /// derivable from `AppState` alone.
    ///
    /// Call this once at startup **after** all config overrides are applied
    /// (e.g. after `apply_telegram_config`), so a config-supplied secret is not
    /// falsely reported as missing. WeCom and MS Teams are intentionally
    /// omitted: their adapters treat the L1 secret as a construction
    /// precondition (`from_env` returns `None` without it) and verify every
    /// request, so they cannot be active-but-unconfigured.
    #[cfg_attr(not(feature = "feishu"), allow(unused_variables))]
    pub fn warn_unenforceable_l1(&self, feishu_webhook_route_mounted: bool) {
        use tracing::warn;
        for (platform, hint) in self.unenforceable_l1(feishu_webhook_route_mounted) {
            warn!(
                platform,
                hint,
                "L1 webhook authentication is NOT configured — this webhook accepts \
                 unauthenticated requests, so the per-platform allowed_users (L3) allowlist \
                 is forgeable: an attacker can POST a spoofed allowlisted sender id and pass \
                 the trust gate. Configure the platform's webhook secret/signature to make \
                 identity trust enforceable. \
                 See https://github.com/openabdev/openab/issues/1356."
            );
        }
    }

    /// The platforms whose L1 is unenforceable right now, with a remediation
    /// hint each. Separated from the warn wrapper so the per-platform
    /// active/configured wiring is unit-testable.
    #[cfg_attr(not(feature = "feishu"), allow(unused_variables))]
    fn unenforceable_l1(
        &self,
        feishu_webhook_route_mounted: bool,
    ) -> Vec<(&'static str, &'static str)> {
        // (platform, active, l1_configured, remediation hint)
        #[allow(unused_mut)]
        let mut checks: Vec<(&str, bool, bool, &str)> = vec![
            (
                "telegram",
                self.telegram_bot_token.is_some(),
                // secret_token is the primary L1; the trusted_source_only IP
                // allowlist is a weaker-but-real alternate L1 (ADR Layer 1).
                self.telegram_secret_token.is_some() || self.telegram_trusted_source_only,
                "set TELEGRAM_SECRET_TOKEN (or [telegram].secret_token), or enable \
                 TELEGRAM_TRUSTED_SOURCE_ONLY",
            ),
            (
                "line",
                // Any LINE env present = an operator intends to run LINE. With
                // no LINE env at all the route still mounts, but spoofed events
                // then face the core trust gate's deny-all default, so we avoid
                // a false-positive warn on gateways that don't use LINE.
                self.line_channel_secret.is_some() || self.line_access_token.is_some(),
                self.line_channel_secret.is_some(),
                "set LINE_CHANNEL_SECRET",
            ),
        ];
        #[cfg(feature = "feishu")]
        checks.push((
            "feishu",
            // Active = the webhook route is actually exposed (caller-supplied:
            // the standalone gateway mounts it only in Webhook connection
            // mode; the unified binary mounts it unconditionally). Websocket
            // delivery itself needs no L1 secret — events arrive over an
            // outbound long-connection.
            self.feishu.is_some() && feishu_webhook_route_mounted,
            self.feishu
                .as_ref()
                .map(|f| f.config.encrypt_key.is_some())
                .unwrap_or(false),
            "set FEISHU_ENCRYPT_KEY",
        ));
        #[cfg(feature = "googlechat")]
        checks.push((
            "googlechat",
            self.google_chat.is_some(),
            self.google_chat
                .as_ref()
                .map(|a| a.jwt_verifier.is_some())
                .unwrap_or(false),
            "set GOOGLE_CHAT_AUDIENCE",
        ));
        checks
            .into_iter()
            .filter(|(_, active, l1_configured, _)| l1_unenforceable(*active, *l1_configured))
            .map(|(platform, _, _, hint)| (platform, hint))
            .collect()
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

    /// Apply resolved `[line]` config values, overriding the env-derived
    /// fields (#1376). Same crate-boundary pattern as
    /// [`AppState::apply_telegram_config`]. Call before
    /// [`AppState::warn_unenforceable_l1`] so a config-supplied
    /// `channel_secret` is not falsely flagged as missing L1.
    pub fn apply_line_config(&mut self, cfg: GatewayLineConfig) {
        self.line_channel_secret = cfg.channel_secret;
        self.line_access_token = cfg.channel_access_token;
        self.line_webhook_path = cfg.webhook_path;
    }

    /// Apply resolved `[wecom]` config values (#1378), rebuilding the WeCom
    /// adapter from them. Reuses the adapter's `from_reader` construction so
    /// the exact same validation applies (all five credentials mandatory,
    /// numeric agent_id, 43-char AES key) — an incomplete section resolves to
    /// no adapter, matching env-only semantics.
    #[cfg(feature = "wecom")]
    pub fn apply_wecom_config(&mut self, cfg: GatewayWecomConfig) {
        let streaming = if cfg.streaming_enabled { "true" } else { "false" }.to_string();
        let debounce = cfg.debounce_secs.to_string();
        self.wecom = adapters::wecom::WecomConfig::from_reader(|k| match k {
            "WECOM_CORP_ID" => cfg.corp_id.clone(),
            "WECOM_SECRET" => cfg.secret.clone(),
            "WECOM_TOKEN" => cfg.token.clone(),
            "WECOM_ENCODING_AES_KEY" => cfg.encoding_aes_key.clone(),
            "WECOM_AGENT_ID" => cfg.agent_id.clone(),
            "WECOM_WEBHOOK_PATH" => Some(cfg.webhook_path.clone()),
            "WECOM_STREAMING_ENABLED" => Some(streaming.clone()),
            "WECOM_DEBOUNCE_SECS" => Some(debounce.clone()),
            _ => None,
        })
        .map(adapters::wecom::WecomAdapter::new);
    }

    /// Apply resolved `[googlechat]` config values (#1379), rebuilding the
    /// adapter from them via the same `from_parts` construction as env-only
    /// startup. Call before [`AppState::warn_unenforceable_l1`] so a
    /// config-supplied `audience` (JWT verifier) is not falsely flagged.
    #[cfg(feature = "googlechat")]
    pub fn apply_googlechat_config(&mut self, cfg: GatewayGoogleChatConfig) {
        self.googlechat_webhook_path = cfg.webhook_path;
        self.google_chat = if cfg.enabled {
            Some(adapters::googlechat::GoogleChatAdapter::from_parts(
                cfg.sa_key_json,
                cfg.sa_key_file,
                cfg.access_token,
                cfg.audience,
            ))
        } else {
            None
        };
    }

    /// Apply resolved `[teams]` config values (#1380), rebuilding the adapter
    /// through the same `from_reader` construction as env-only startup
    /// (app_id + app_secret mandatory; incomplete section disables the
    /// adapter, matching env-only semantics).
    #[cfg(feature = "teams")]
    pub fn apply_teams_config(&mut self, cfg: GatewayTeamsConfig) {
        self.teams_webhook_path = cfg.webhook_path;
        let tenants = cfg.allowed_tenants.join(",");
        self.teams = adapters::teams::TeamsConfig::from_reader(|k| match k {
            "TEAMS_APP_ID" => cfg.app_id.clone(),
            "TEAMS_APP_SECRET" => cfg.app_secret.clone(),
            "TEAMS_OAUTH_ENDPOINT" => Some(cfg.oauth_endpoint.clone()),
            "TEAMS_OPENID_METADATA" => Some(cfg.openid_metadata.clone()),
            "TEAMS_ALLOWED_TENANTS" => Some(tenants.clone()),
            _ => None,
        })
        .map(adapters::teams::TeamsAdapter::new);
    }

    /// Apply resolved `[feishu]` config values (#1377), rebuilding the
    /// adapter through the same `from_reader` construction as env-only
    /// startup (app_id + app_secret mandatory; incomplete section disables
    /// the adapter). Call before [`AppState::warn_unenforceable_l1`] so a
    /// config-supplied `encrypt_key` is not falsely flagged.
    #[cfg(feature = "feishu")]
    pub fn apply_feishu_config(&mut self, cfg: GatewayFeishuConfig) {
        self.feishu = adapters::feishu::FeishuConfig::from_reader(|k| cfg.pairs.get(k).cloned())
            .map(adapters::feishu::FeishuAdapter::new);
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

/// Parameter object for passing resolved LINE config across the crate
/// boundary without introducing a dependency on `openab-core` (#1376).
#[derive(Debug, Clone)]
pub struct GatewayLineConfig {
    pub channel_secret: Option<String>,
    pub channel_access_token: Option<String>,
    pub webhook_path: String,
}

/// Parameter object for passing resolved WeCom config across the crate
/// boundary without introducing a dependency on `openab-core` (#1378).
/// Fields are the fully resolved (config → env → default) values.
#[derive(Debug, Clone)]
pub struct GatewayWecomConfig {
    pub corp_id: Option<String>,
    pub secret: Option<String>,
    pub token: Option<String>,
    pub encoding_aes_key: Option<String>,
    pub agent_id: Option<String>,
    pub webhook_path: String,
    pub streaming_enabled: bool,
    pub debounce_secs: u64,
}

/// Parameter object for passing resolved Google Chat config across the crate
/// boundary without introducing a dependency on `openab-core` (#1379).
#[derive(Debug, Clone)]
pub struct GatewayGoogleChatConfig {
    pub enabled: bool,
    pub sa_key_json: Option<String>,
    pub sa_key_file: Option<String>,
    pub access_token: Option<String>,
    pub audience: Option<String>,
    pub webhook_path: String,
}

/// Parameter object for passing resolved Teams config across the crate
/// boundary without introducing a dependency on `openab-core` (#1380).
#[derive(Debug, Clone)]
pub struct GatewayTeamsConfig {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub allowed_tenants: Vec<String>,
    pub oauth_endpoint: String,
    pub openid_metadata: String,
    pub webhook_path: String,
}

/// Parameter object for passing resolved Feishu config across the crate
/// boundary without introducing a dependency on `openab-core` (#1377).
/// Carries the config-first key/value pairs in env-var string form; the
/// adapter's `from_reader` performs all parsing and default resolution.
#[derive(Debug, Clone)]
pub struct GatewayFeishuConfig {
    pub pairs: std::collections::HashMap<String, String>,
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

    // ACP Server adapter
    #[cfg(feature = "acp")]
    if std::env::var("OPENAB_ACP_ENABLED").map(|v| v == "true" || v == "1").unwrap_or(false) {
        info!("ACP server endpoint enabled at /acp");
        app = app.route("/acp", get(adapters::acp_server::ws_upgrade));
    }

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
        // Missing-secret warning is emitted by warn_unenforceable_l1 below,
        // which also accounts for the trusted_source_only IP-allowlist L1.
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
    let line_webhook_path =
        std::env::var("LINE_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/line".into());
    #[cfg(feature = "line")]
    {
        info!(path = %line_webhook_path, "line adapter enabled");
        app = app.route(&line_webhook_path, post(adapters::line::webhook));
    }
    #[cfg(not(feature = "line"))]
    let line_channel_secret: Option<String> = None;
    #[cfg(not(feature = "line"))]
    let line_access_token: Option<String> = None;
    #[cfg(not(feature = "line"))]
    let line_webhook_path = "/webhook/line".to_string();

    // Teams adapter
    #[cfg(feature = "teams")]
    let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
        info!("teams adapter enabled");
        adapters::teams::TeamsAdapter::new(config)
    });
    #[cfg(not(feature = "teams"))]
    let teams: Option<()> = None;

    let teams_webhook_path =
        std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());
    #[cfg(feature = "teams")]
    if teams.is_some() {
        info!(path = %teams_webhook_path, "teams webhook registered");
        app = app.route(&teams_webhook_path, post(adapters::teams::webhook));
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
    let googlechat_webhook_path = std::env::var("GOOGLE_CHAT_WEBHOOK_PATH")
        .unwrap_or_else(|_| "/webhook/googlechat".into());
    #[cfg(feature = "googlechat")]
    let google_chat = {
        let enabled = std::env::var("GOOGLE_CHAT_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if enabled {
            info!(path = %googlechat_webhook_path, "googlechat adapter enabled");
            app = app.route(&googlechat_webhook_path, post(adapters::googlechat::webhook));
            Some(adapters::googlechat::GoogleChatAdapter::from_parts(
                std::env::var("GOOGLE_CHAT_SA_KEY_JSON").ok(),
                std::env::var("GOOGLE_CHAT_SA_KEY_FILE").ok(),
                std::env::var("GOOGLE_CHAT_ACCESS_TOKEN").ok(),
                std::env::var("GOOGLE_CHAT_AUDIENCE").ok(),
            ))
        } else {
            None
        }
    };
    #[cfg(not(feature = "googlechat"))]
    let google_chat: Option<()> = None;
    #[cfg(not(feature = "googlechat"))]
    let googlechat_webhook_path = "/webhook/googlechat".to_string();

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

    // ACP server — extract once so the config and its reply registry share a
    // single from_env() result (a registry is only created when ACP is enabled).
    #[cfg(feature = "acp")]
    let acp = adapters::acp_server::AcpConfig::from_env();
    #[cfg(feature = "acp")]
    let acp_reply_registry = acp
        .as_ref()
        .map(|_| adapters::acp_server::new_reply_registry());

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
        line_webhook_path,
        #[cfg(feature = "teams")]
        teams,
        teams_webhook_path,
        teams_service_urls: Mutex::new(HashMap::new()),
        #[cfg(feature = "feishu")]
        feishu,
        #[cfg(feature = "googlechat")]
        google_chat,
        googlechat_webhook_path,
        #[cfg(feature = "wecom")]
        wecom,
        #[cfg(feature = "acp")]
        acp,
        #[cfg(feature = "acp")]
        acp_reply_registry,
        ws_token,
        event_tx,
        reply_token_cache,
        line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
        client,
    });

    // Phase 1 L1 audit (#1356): warn if any active webhook platform has no
    // transport authentication configured (identity trust unenforceable).
    // The standalone gateway mounts the feishu webhook route only in Webhook
    // connection mode (see the route setup above).
    #[cfg(feature = "feishu")]
    let feishu_webhook_route_mounted = state
        .feishu
        .as_ref()
        .map(|f| f.config.connection_mode == adapters::feishu::ConnectionMode::Webhook)
        .unwrap_or(false);
    #[cfg(not(feature = "feishu"))]
    let feishu_webhook_route_mounted = false;
    state.warn_unenforceable_l1(feishu_webhook_route_mounted);

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
                            #[cfg(feature = "acp")]
                            "acp" => {
                                if let Some(ref registry) = state_for_recv.acp_reply_registry {
                                    adapters::acp_server::handle_reply(&reply, registry).await;
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

#[cfg(test)]
mod l1_audit_tests {
    use super::{l1_unenforceable, AppState};
    use tokio::sync::broadcast;

    #[test]
    fn warns_only_when_active_and_secret_missing() {
        // active platform, no L1 secret → unenforceable (warn)
        assert!(l1_unenforceable(true, false));
        // active with L1 configured → fine
        assert!(!l1_unenforceable(true, true));
        // inactive platform → never warn, regardless of L1
        assert!(!l1_unenforceable(false, false));
        assert!(!l1_unenforceable(false, true));
    }

    fn state() -> AppState {
        let (tx, _rx) = broadcast::channel(4);
        AppState::test_default(tx)
    }

    fn flagged(s: &AppState) -> Vec<&'static str> {
        s.unenforceable_l1(false)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn inactive_platforms_are_never_flagged() {
        // test_default is all-None → nothing configured, nothing active.
        assert!(flagged(&state()).is_empty());
        // …even when a feishu webhook route is reported as mounted (no adapter).
        assert!(state().unenforceable_l1(true).is_empty());
    }

    #[test]
    fn telegram_active_without_l1_is_flagged() {
        let mut s = state();
        s.telegram_bot_token = Some("bot".into());
        assert_eq!(flagged(&s), vec!["telegram"]);

        // secret_token satisfies L1
        s.telegram_secret_token = Some("sec".into());
        assert!(flagged(&s).is_empty());

        // trusted_source_only is an accepted alternate L1
        s.telegram_secret_token = None;
        s.telegram_trusted_source_only = true;
        assert!(flagged(&s).is_empty());
    }

    #[test]
    fn line_flagged_only_when_active_without_secret() {
        let mut s = state();
        // access token present but no channel secret → active, L1 missing
        s.line_access_token = Some("tok".into());
        assert_eq!(flagged(&s), vec!["line"]);

        // channel secret present → L1 enforced
        s.line_channel_secret = Some("csecret".into());
        assert!(flagged(&s).is_empty());
    }

    #[cfg(feature = "feishu")]
    #[test]
    fn apply_feishu_config_requires_credentials() {
        use super::GatewayFeishuConfig;
        use std::collections::HashMap;
        let mut s = state();
        // Complete credentials → adapter built via the shared from_reader.
        let mut pairs = HashMap::new();
        pairs.insert("FEISHU_APP_ID".to_string(), "cli_x".to_string());
        pairs.insert("FEISHU_APP_SECRET".to_string(), "sec".to_string());
        pairs.insert("FEISHU_CONNECTION_MODE".to_string(), "webhook".to_string());
        pairs.insert("FEISHU_ENCRYPT_KEY".to_string(), "ek".to_string());
        s.apply_feishu_config(GatewayFeishuConfig {
            pairs: pairs.clone(),
        });
        assert!(s.feishu.is_some());
        let cfg = &s.feishu.as_ref().unwrap().config;
        assert_eq!(cfg.app_id, "cli_x");
        assert!(matches!(
            cfg.connection_mode,
            super::adapters::feishu::ConnectionMode::Webhook
        ));
        assert_eq!(cfg.encrypt_key.as_deref(), Some("ek"));
        // Config-supplied encrypt_key satisfies the L1 startup check
        // when the webhook route is exposed.
        assert!(s.unenforceable_l1(true).is_empty());

        // Missing secret → adapter disabled.
        pairs.remove("FEISHU_APP_SECRET");
        s.apply_feishu_config(GatewayFeishuConfig { pairs });
        assert!(s.feishu.is_none());
    }

    #[cfg(feature = "teams")]
    #[test]
    fn apply_teams_config_requires_credentials() {
        use super::GatewayTeamsConfig;
        let mut s = state();
        // Complete credentials → adapter built, path set.
        s.apply_teams_config(GatewayTeamsConfig {
            app_id: Some("app".into()),
            app_secret: Some("sec".into()),
            allowed_tenants: vec!["t1".into()],
            oauth_endpoint: "https://x/token".into(),
            openid_metadata: "https://x/oidc".into(),
            webhook_path: "/hook/teams".into(),
        });
        assert!(s.teams.is_some());
        assert_eq!(s.teams_webhook_path, "/hook/teams");

        // Missing secret → adapter disabled (same as env-only semantics).
        s.apply_teams_config(GatewayTeamsConfig {
            app_id: Some("app".into()),
            app_secret: None,
            allowed_tenants: vec![],
            oauth_endpoint: "https://x/token".into(),
            openid_metadata: "https://x/oidc".into(),
            webhook_path: "/hook/teams".into(),
        });
        assert!(s.teams.is_none());
    }

    #[cfg(feature = "googlechat")]
    #[test]
    fn apply_googlechat_config_builds_adapter_and_feeds_l1_warning() {
        use super::GatewayGoogleChatConfig;
        let mut s = state();
        // Enabled without audience → adapter active, no JWT verifier → flagged.
        s.apply_googlechat_config(GatewayGoogleChatConfig {
            enabled: true,
            sa_key_json: None,
            sa_key_file: None,
            access_token: Some("tok".into()),
            audience: None,
            webhook_path: "/hook/gc".into(),
        });
        assert!(s.google_chat.is_some());
        assert_eq!(s.googlechat_webhook_path, "/hook/gc");
        assert_eq!(flagged(&s), vec!["googlechat"]);

        // Config-supplied audience builds the verifier → L1 satisfied.
        s.apply_googlechat_config(GatewayGoogleChatConfig {
            enabled: true,
            sa_key_json: None,
            sa_key_file: None,
            access_token: Some("tok".into()),
            audience: Some("aud".into()),
            webhook_path: "/hook/gc".into(),
        });
        assert!(flagged(&s).is_empty());

        // Disabled → adapter removed.
        s.apply_googlechat_config(GatewayGoogleChatConfig {
            enabled: false,
            sa_key_json: None,
            sa_key_file: None,
            access_token: None,
            audience: None,
            webhook_path: "/hook/gc".into(),
        });
        assert!(s.google_chat.is_none());
    }

    #[test]
    fn apply_line_config_overrides_env_state_and_feeds_l1_warning() {
        use super::GatewayLineConfig;
        let mut s = state();
        // Simulate env-derived state: token from env, no secret → flagged.
        s.line_access_token = Some("env-tok".into());
        assert_eq!(flagged(&s), vec!["line"]);

        // Config-first override (#1376): [line] supplies the secret + path.
        s.apply_line_config(GatewayLineConfig {
            channel_secret: Some("cfg-secret".into()),
            channel_access_token: Some("cfg-tok".into()),
            webhook_path: "/hook/line".into(),
        });
        assert_eq!(s.line_channel_secret.as_deref(), Some("cfg-secret"));
        assert_eq!(s.line_access_token.as_deref(), Some("cfg-tok"));
        assert_eq!(s.line_webhook_path, "/hook/line");
        // Config-supplied secret satisfies the L1 startup check.
        assert!(flagged(&s).is_empty());
    }
}

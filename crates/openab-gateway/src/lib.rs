pub mod adapters;
pub(crate) mod media;
pub mod schema;
pub mod store;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Maximum number of post-ack LINE WORKS webhook events processed concurrently.
pub const LINEWORKS_WEBHOOK_CONCURRENCY_MAX: usize = 8;

/// Maximum number of accepted LINE WORKS callbacks allowed to wait for a
/// worker permit. Bursts up to this depth are acknowledged and queued instead
/// of rejected (LINE WORKS does not resend callbacks); beyond it the webhook
/// answers 503 — a loud, bounded overflow.
pub const LINEWORKS_INGRESS_QUEUE_MAX: usize = 64;

// --- App state (shared across all adapters) ---

/// Pre-download identity probe installed by the unified binary: maps
/// `(platform, channel_id, sender_id)` to "may this sender consume adapter
/// resources?". Lets adapters apply the core L3 identity gate BEFORE
/// expensive work (attachment download) without a crate dependency on
/// openab-core. `None` = probe unavailable (standalone gateway) — adapters
/// proceed and the core-side ingress gate still applies after broadcast.
pub type IngressTrustProbe = Arc<dyn Fn(&str, &str, &str) -> bool + Send + Sync>;

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
    #[cfg(feature = "acp")]
    pub acp_tunnel_registry: Option<adapters::acp_server::AcpTunnelRegistry>,
    #[cfg(feature = "lineworks")]
    pub lineworks: Option<Arc<adapters::lineworks::LineWorksAdapter>>,
    pub ws_token: Option<String>,
    pub event_tx: broadcast::Sender<String>,
    /// Number of active OAB WebSocket consumers. M0 supports one; additional
    /// consumers are admitted for compatibility but marked unsupported.
    pub active_oab_consumers: Arc<AtomicUsize>,
    pub reply_token_cache: ReplyTokenCache,
    pub line_webhook_semaphore: Arc<Semaphore>,
    /// Bounds post-ack LINE WORKS webhook processing (mention gate + attachment download).
    pub lineworks_webhook_semaphore: Arc<Semaphore>,
    /// Bounds LINE WORKS callbacks accepted but still waiting for a worker
    /// permit — burst absorption (see [`LINEWORKS_INGRESS_QUEUE_MAX`]).
    pub lineworks_ingress_queue: Arc<Semaphore>,
    /// Optional pre-download identity probe (see [`IngressTrustProbe`]).
    pub trust_probe: Option<IngressTrustProbe>,
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
            #[cfg(feature = "acp")]
            acp_tunnel_registry: None,
            #[cfg(feature = "lineworks")]
            lineworks: None,
            ws_token: None,
            event_tx,
            active_oab_consumers: Arc::new(AtomicUsize::new(0)),
            reply_token_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
            lineworks_webhook_semaphore: Arc::new(Semaphore::new(
                LINEWORKS_WEBHOOK_CONCURRENCY_MAX,
            )),
            lineworks_ingress_queue: Arc::new(Semaphore::new(LINEWORKS_INGRESS_QUEUE_MAX)),
            trust_probe: None,
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
        #[cfg(feature = "acp")]
        let acp_tunnel_registry = acp.as_ref().map(|_| adapters::acp_server::new_tunnel_registry());
        // LINE WORKS
        #[cfg(feature = "lineworks")]
        let lineworks = adapters::lineworks::LineWorksConfig::from_env().map(|config| {
            info!("lineworks adapter configured");
            Arc::new(adapters::lineworks::LineWorksAdapter::new(config))
        });

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
            #[cfg(feature = "acp")]
            acp_tunnel_registry,
            #[cfg(feature = "lineworks")]
            lineworks,
            ws_token,
            event_tx,
            active_oab_consumers: Arc::new(AtomicUsize::new(0)),
            reply_token_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
        lineworks_webhook_semaphore: Arc::new(Semaphore::new(LINEWORKS_WEBHOOK_CONCURRENCY_MAX)),
        lineworks_ingress_queue: Arc::new(Semaphore::new(LINEWORKS_INGRESS_QUEUE_MAX)),
        trust_probe: None,
            client,
        }
    }

    /// Capabilities advertised to a new Core during the optional WebSocket
    /// hello exchange. Only configured adapters are included, and operation ACK
    /// flags are conservative: a platform is advertised only when its current
    /// handler emits a GatewayResponse for that operation.
    pub fn gateway_capabilities(&self) -> HashMap<String, schema::AdapterCapabilities> {
        use schema::{AdapterCapabilities, MessageLimit, StatusBackend, StreamingMode};

        let mut capabilities = HashMap::new();
        let mut insert = |platform: &str, value: AdapterCapabilities| {
            capabilities.insert(platform.to_string(), value);
        };
        let characters = |max| MessageLimit::Characters { max };

        if self.telegram_bot_token.is_some() {
            insert(
                "telegram",
                AdapterCapabilities {
                    can_edit: self.telegram_rich_messages,
                    streaming_mode: if self.telegram_rich_messages {
                        StreamingMode::Edit
                    } else {
                        StreamingMode::Disabled
                    },
                    show_streaming_placeholder: !self.telegram_rich_messages,
                    message_limit: characters(4096),
                    status_backend: StatusBackend::Reactions,
                    ..AdapterCapabilities::default()
                },
            );
        }
        if self.line_access_token.is_some() {
            insert(
                "line",
                AdapterCapabilities {
                    message_limit: characters(4096),
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "teams")]
        if self.teams.is_some() {
            insert(
                "teams",
                AdapterCapabilities {
                    show_streaming_placeholder: true,
                    message_limit: characters(4096),
                    status_backend: StatusBackend::None,
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "feishu")]
        if self.feishu.is_some() {
            insert(
                "feishu",
                AdapterCapabilities {
                    send_ack: true,
                    edit_ack: true,
                    delete_ack: true,
                    can_edit: true,
                    can_delete: true,
                    streaming_mode: StreamingMode::Edit,
                    message_limit: characters(4096),
                    status_backend: StatusBackend::Reactions,
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "googlechat")]
        if self.google_chat.is_some() {
            insert(
                "googlechat",
                AdapterCapabilities {
                    send_ack: true,
                    can_edit: true,
                    streaming_mode: StreamingMode::Edit,
                    message_limit: characters(4096),
                    status_backend: StatusBackend::Reactions,
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "wecom")]
        if self.wecom.is_some() {
            insert(
                "wecom",
                AdapterCapabilities {
                    message_limit: characters(2048),
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "lineworks")]
        if self.lineworks.is_some() {
            insert(
                "lineworks",
                AdapterCapabilities {
                    message_limit: characters(2000),
                    ..AdapterCapabilities::default()
                },
            );
        }
        #[cfg(feature = "acp")]
        if self.acp.is_some() {
            insert(
                "acp",
                AdapterCapabilities {
                    message_limit: MessageLimit::Unlimited,
                    show_streaming_placeholder: false,
                    ..AdapterCapabilities::default()
                },
            );
        }
        capabilities
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

    /// Apply resolved `[lineworks]` config values, rebuilding the adapter
    /// through the same `from_reader` validation as env-only construction —
    /// an incomplete section resolves to no adapter, matching env-only
    /// semantics. Same crate-boundary pattern as
    /// [`AppState::apply_wecom_config`].
    #[cfg(feature = "lineworks")]
    pub fn apply_lineworks_config(&mut self, cfg: GatewayLineWorksConfig) {
        self.lineworks = adapters::lineworks::LineWorksConfig::from_reader(|k| match k {
            "LINEWORKS_BOT_ID" => cfg.bot_id.clone(),
            "LINEWORKS_BOT_SECRET" => cfg.bot_secret.clone(),
            "LINEWORKS_CLIENT_ID" => cfg.client_id.clone(),
            "LINEWORKS_CLIENT_SECRET" => cfg.client_secret.clone(),
            "LINEWORKS_SERVICE_ACCOUNT" => cfg.service_account.clone(),
            "LINEWORKS_PRIVATE_KEY" => cfg.private_key.clone(),
            "LINEWORKS_PRIVATE_KEY_FILE" => cfg.private_key_file.clone(),
            "LINEWORKS_WEBHOOK_PATH" => Some(cfg.webhook_path.clone()),
            "LINEWORKS_REQUIRE_MENTION" => Some(cfg.require_mention.to_string()),
            "LINEWORKS_BOT_NAME" => cfg.bot_name.clone(),
            "LINEWORKS_RICH_MESSAGES" => Some(cfg.rich_messages.to_string()),
            "LINEWORKS_ACK_MESSAGE" => cfg.ack_message.clone(),
            _ => None,
        })
        .map(|config| Arc::new(adapters::lineworks::LineWorksAdapter::new(config)));
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
        let dedupe_ttl_secs = cfg.dedupe_ttl_secs.to_string();
        let route_ttl_secs = cfg.route_ttl_secs.to_string();
        let max_route_entries = cfg.max_route_entries.to_string();
        self.teams = adapters::teams::TeamsConfig::from_reader(|k| match k {
            "TEAMS_APP_ID" => cfg.app_id.clone(),
            "TEAMS_APP_SECRET" => cfg.app_secret.clone(),
            "TEAMS_OAUTH_ENDPOINT" => Some(cfg.oauth_endpoint.clone()),
            "TEAMS_OPENID_METADATA" => Some(cfg.openid_metadata.clone()),
            "TEAMS_ALLOWED_TENANTS" => Some(tenants.clone()),
            "TEAMS_DEDUPE_TTL_SECS" => Some(dedupe_ttl_secs.clone()),
            "TEAMS_ROUTE_TTL_SECS" => Some(route_ttl_secs.clone()),
            "TEAMS_MAX_ROUTE_ENTRIES" => Some(max_route_entries.clone()),
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

/// Parameter object for passing resolved LINE WORKS config across the crate
/// boundary without introducing a dependency on `openab-core`.
/// Fields are the fully resolved (config → env → default) values.
#[derive(Debug, Clone)]
pub struct GatewayLineWorksConfig {
    pub bot_id: Option<String>,
    pub bot_secret: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub service_account: Option<String>,
    pub private_key: Option<String>,
    pub private_key_file: Option<String>,
    pub webhook_path: String,
    pub require_mention: bool,
    pub bot_name: Option<String>,
    pub rich_messages: bool,
    pub ack_message: Option<String>,
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
    pub dedupe_ttl_secs: u64,
    pub route_ttl_secs: u64,
    pub max_route_entries: usize,
}

/// Start the shared Teams state sweeper for Standalone or Unified mode.
#[cfg(feature = "teams")]
pub fn spawn_teams_ingress_cleanup(state: Arc<AppState>) {
    const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            let Some(teams) = state.teams.as_ref() else {
                continue;
            };

            let stats = teams.cleanup_ingress().await;
            let now = Instant::now();
            let route_ttl = teams.route_ttl();
            let mut service_urls = state.teams_service_urls.lock().await;
            let service_urls_before = service_urls.len();
            service_urls.retain(|_, (_, inserted_at)| {
                now.saturating_duration_since(*inserted_at) < route_ttl
            });
            let service_urls_removed = service_urls_before - service_urls.len();

            if stats.routes_removed > 0
                || stats.dedupe_entries_removed > 0
                || stats.stale_publications_removed > 0
                || service_urls_removed > 0
            {
                tracing::info!(
                    routes_removed = stats.routes_removed,
                    dedupe_entries_removed = stats.dedupe_entries_removed,
                    stale_publications_removed = stats.stale_publications_removed,
                    service_urls_removed,
                    "teams ingress state cleanup"
                );
            }
        }
    });
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

    // ACP Server adapter. Fail-open (no transport key) is only allowed on a loopback
    // bind; a non-loopback bind without OPENAB_ACP_AUTH_KEY refuses to mount /acp.
    #[cfg(feature = "acp")]
    if std::env::var("OPENAB_ACP_ENABLED").map(|v| v == "true" || v == "1").unwrap_or(false) {
        let acp_key = std::env::var("OPENAB_ACP_AUTH_KEY").ok();
        match adapters::acp_server::acp_auth_ok_for_bind(acp_key.as_deref(), &listen_addr) {
            Ok(()) => {
                info!("ACP server endpoint enabled at /acp");
                app = app.route("/acp", get(adapters::acp_server::ws_upgrade));
            }
            Err(e) => tracing::error!("ACP endpoint NOT mounted: {e}"),
        }
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
    #[cfg(feature = "acp")]
    let acp_tunnel_registry = acp
        .as_ref()
        .map(|_| adapters::acp_server::new_tunnel_registry());
    // LINE WORKS adapter
    #[cfg(feature = "lineworks")]
    let lineworks = adapters::lineworks::LineWorksConfig::from_env()
        .map(|config| Arc::new(adapters::lineworks::LineWorksAdapter::new(config)));
    #[cfg(feature = "lineworks")]
    if let Some(ref lw) = lineworks {
        let path = lw.config.webhook_path.clone();
        info!(path = %path, "lineworks adapter enabled");
        app = app.route(&path, post(adapters::lineworks::webhook));
    }

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
        #[cfg(feature = "acp")]
        acp_tunnel_registry,
        #[cfg(feature = "lineworks")]
        lineworks,
        ws_token,
        event_tx,
        active_oab_consumers: Arc::new(AtomicUsize::new(0)),
        reply_token_cache,
        line_webhook_semaphore: Arc::new(Semaphore::new(LINE_WEBHOOK_CONCURRENCY_MAX)),
        lineworks_webhook_semaphore: Arc::new(Semaphore::new(LINEWORKS_WEBHOOK_CONCURRENCY_MAX)),
        lineworks_ingress_queue: Arc::new(Semaphore::new(LINEWORKS_INGRESS_QUEUE_MAX)),
        trust_probe: None,
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

    // Background: sweep bounded Teams route, dedupe, and compatibility state.
    #[cfg(feature = "teams")]
    spawn_teams_ingress_cleanup(state.clone());

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

struct ActiveConsumerGuard {
    counter: Arc<AtomicUsize>,
}

impl ActiveConsumerGuard {
    fn enter(counter: Arc<AtomicUsize>) -> (Self, usize) {
        let active = counter.fetch_add(1, Ordering::AcqRel) + 1;
        (Self { counter }, active)
    }
}

impl Drop for ActiveConsumerGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

fn build_gateway_hello(
    state: &AppState,
    client_hello: &schema::GatewayClientHello,
) -> schema::GatewayHello {
    let mut capabilities = state.gateway_capabilities();
    if !client_hello.requested_platforms.is_empty() {
        capabilities.retain(|platform, _| {
            client_hello
                .requested_platforms
                .iter()
                .any(|requested| requested == platform)
        });
    }
    let active_consumers = state.active_oab_consumers.load(Ordering::Acquire);
    schema::GatewayHello {
        schema: schema::GATEWAY_HELLO_SCHEMA.into(),
        protocol_version: schema::GATEWAY_PROTOCOL_VERSION,
        capabilities,
        topology: schema::GatewayTopology {
            active_consumers,
            supported: active_consumers == 1,
            delivery_mode: "best_effort_broadcast".into(),
        },
    }
}

async fn handle_oab_connection(state: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    use tracing::{error, info, warn};

    let (_consumer_guard, active_consumers) =
        ActiveConsumerGuard::enter(state.active_oab_consumers.clone());
    if active_consumers > 1 {
        error!(
            active_consumers,
            topology_supported = false,
            "multiple active OAB consumers detected; M0 broadcast topology is unsupported and may duplicate events"
        );
    }

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<String>(4);

    info!(active_consumers, "OAB client connected via WebSocket");

    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                Some(control_json) = control_rx.recv() => {
                    if ws_tx.send(Message::Text(control_json.into())).await.is_err() {
                        break;
                    }
                }
                event = event_rx.recv() => {
                    match event {
                        Ok(event_json) => {
                            if ws_tx.send(Message::Text(event_json.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "OAB consumer lagged gateway broadcast; events were lost");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    let state_for_recv = state.clone();
    let reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut recv_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                if let Ok(envelope) = serde_json::from_str::<schema::GatewayEnvelope>(&text) {
                    if envelope.schema == schema::CLIENT_HELLO_SCHEMA {
                        match serde_json::from_str::<schema::GatewayClientHello>(&text) {
                            Ok(client_hello) => {
                                if client_hello.protocol_version != schema::GATEWAY_PROTOCOL_VERSION {
                                    warn!(
                                        client_version = client_hello.protocol_version,
                                        gateway_version = schema::GATEWAY_PROTOCOL_VERSION,
                                        "gateway protocol version differs; responding with supported version"
                                    );
                                }
                                let hello = build_gateway_hello(&state_for_recv, &client_hello);
                                if let Ok(json) = serde_json::to_string(&hello) {
                                    if control_tx.send(json).await.is_err() {
                                        break;
                                    }
                                }
                                continue;
                            }
                            Err(error) => {
                                warn!(error = %error, "invalid gateway client hello");
                                continue;
                            }
                        }
                    }
                }

                match serde_json::from_str::<schema::GatewayReply>(&text) {
                    Ok(reply) => {
                        info!(
                            platform = %reply.platform,
                            channel = %redact_channel(&reply.channel.id),
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
                                    if let Err(e) = adapters::teams::handle_reply(
                                        &reply,
                                        teams,
                                        &state_for_recv.teams_service_urls,
                                    )
                                    .await
                                    {
                                        tracing::error!(
                                            error = %e,
                                            command = ?reply.command.as_deref(),
                                            "teams reply rejected"
                                        );
                                    }
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
                            #[cfg(feature = "lineworks")]
                            "lineworks" => {
                                if let Some(ref lineworks) = state_for_recv.lineworks {
                                    let ok = adapters::lineworks::dispatch_lineworks_reply(
                                        &client,
                                        lineworks,
                                        &reply,
                                    )
                                    .await;
                                    if !ok {
                                        tracing::error!(
                                            channel = %reply.channel.id,
                                            command = ?reply.command.as_deref(),
                                            "lineworks reply delivery failed — reply lost"
                                        );
                                    }
                                } else {
                                    warn!("reply for lineworks but adapter not configured");
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
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
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
        let cfg = &s
            .feishu
            .as_ref()
            .expect("complete Feishu credentials should build an adapter")
            .config;
        assert_eq!(cfg.app_id, "cli_x");
        assert!(matches!(
            cfg.connection_mode,
            super::adapters::feishu::ConnectionMode::Webhook
        ));
        assert_eq!(cfg.encrypt_key.as_deref(), Some("ek"));
        // Config-supplied encrypt_key satisfies the L1 startup check
        // when the webhook route is exposed.
        assert!(s.unenforceable_l1(true).is_empty());
        let capabilities = s.gateway_capabilities();
        let feishu = capabilities
            .get("feishu")
            .expect("configured Feishu adapter should advertise capabilities");
        assert!(feishu.send_ack);
        assert!(feishu.edit_ack);
        assert!(feishu.delete_ack);
        assert_eq!(feishu.streaming_mode, super::schema::StreamingMode::Edit);

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
            dedupe_ttl_secs: 600,
            route_ttl_secs: 3600,
            max_route_entries: 10_000,
        });
        assert!(s.teams.is_some());
        assert_eq!(s.teams_webhook_path, "/hook/teams");
        let capabilities = s.gateway_capabilities();
        let teams = capabilities
            .get("teams")
            .expect("configured Teams adapter should advertise capabilities");
        assert!(!teams.send_ack);
        assert!(!teams.can_edit);
        assert_eq!(teams.streaming_mode, super::schema::StreamingMode::Disabled);
        assert!(teams.show_streaming_placeholder);
        assert_eq!(teams.status_backend, super::schema::StatusBackend::None);

        // Missing secret → adapter disabled (same as env-only semantics).
        s.apply_teams_config(GatewayTeamsConfig {
            app_id: Some("app".into()),
            app_secret: None,
            allowed_tenants: vec![],
            oauth_endpoint: "https://x/token".into(),
            openid_metadata: "https://x/oidc".into(),
            webhook_path: "/hook/teams".into(),
            dedupe_ttl_secs: 600,
            route_ttl_secs: 3600,
            max_route_entries: 10_000,
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

#[cfg(test)]
mod gateway_protocol_tests {
    use super::*;
    use anyhow::Context as _;
    use axum::{routing::get, Router};
    use futures_util::{SinkExt, StreamExt};
    use tokio::time::{sleep, timeout, Duration};
    use tokio_tungstenite::tungstenite::Message;

    type TestSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn start_server(
        state: AppState,
    ) -> anyhow::Result<(
        std::net::SocketAddr,
        Arc<AppState>,
        tokio::task::JoinHandle<()>,
    )> {
        let state = Arc::new(state);
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("loopback test server should run");
        });
        Ok((addr, state, task))
    }

    async fn wait_for_consumers(state: &AppState, expected: usize) -> anyhow::Result<()> {
        timeout(Duration::from_secs(1), async {
            loop {
                if state.active_oab_consumers.load(Ordering::Acquire) == expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(())
    }

    async fn next_text(socket: &mut TestSocket) -> anyhow::Result<String> {
        let message = timeout(Duration::from_secs(1), socket.next())
            .await?
            .context("test WebSocket closed before a frame arrived")??;
        Ok(message.into_text()?)
    }

    #[tokio::test]
    async fn hello_advertises_requested_capabilities_and_topology() -> anyhow::Result<()> {
        let (event_tx, _event_rx) = broadcast::channel(8);
        let mut app_state = AppState::test_default(event_tx);
        app_state.telegram_bot_token = Some("bot-token".into());
        app_state.telegram_rich_messages = true;
        app_state.line_access_token = Some("line-token".into());
        let (addr, state, server) = start_server(app_state).await?;
        let url = format!("{}://{addr}/ws", "ws");

        let (mut first, _) = tokio_tungstenite::connect_async(&url).await?;
        let client_hello = schema::GatewayClientHello {
            schema: schema::CLIENT_HELLO_SCHEMA.into(),
            protocol_version: schema::GATEWAY_PROTOCOL_VERSION,
            client_name: Some("test-core".into()),
            requested_platforms: vec!["telegram".into()],
        };
        first
            .send(Message::Text(serde_json::to_string(&client_hello)?))
            .await?;
        let text = next_text(&mut first).await?;
        let hello: schema::GatewayHello = serde_json::from_str(&text)?;
        assert_eq!(hello.protocol_version, schema::GATEWAY_PROTOCOL_VERSION);
        assert_eq!(hello.capabilities.len(), 1);
        let telegram = hello
            .capabilities
            .get("telegram")
            .context("telegram capability should be advertised")?;
        assert_eq!(telegram.streaming_mode, schema::StreamingMode::Edit);
        assert!(!telegram.show_streaming_placeholder);
        assert!(hello.topology.supported);
        assert_eq!(hello.topology.active_consumers, 1);

        let (mut second, _) = tokio_tungstenite::connect_async(&url).await?;
        second
            .send(Message::Text(serde_json::to_string(&client_hello)?))
            .await?;
        let text = next_text(&mut second).await?;
        let hello: schema::GatewayHello = serde_json::from_str(&text)?;
        assert!(!hello.topology.supported);
        assert_eq!(hello.topology.active_consumers, 2);
        assert_eq!(hello.topology.delivery_mode, "best_effort_broadcast");

        second.close(None).await?;
        wait_for_consumers(&state, 1).await?;
        first.close(None).await?;
        wait_for_consumers(&state, 0).await?;
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn legacy_client_can_send_reply_without_hello() -> anyhow::Result<()> {
        let (event_tx, _event_rx) = broadcast::channel(8);
        let app_state = AppState::test_default(event_tx);
        let (addr, state, server) = start_server(app_state).await?;
        let url = format!("{}://{addr}/ws", "ws");
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await?;
        wait_for_consumers(&state, 1).await?;

        let legacy_reply = schema::GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: "evt-1".into(),
            platform: "unknown".into(),
            channel: schema::ReplyChannel {
                id: "channel-1".into(),
                thread_id: None,
            },
            content: schema::Content {
                content_type: "text".into(),
                text: "hello".into(),
                attachments: Vec::new(),
            },
            command: None,
            request_id: None,
            quote_message_id: None,
        };
        socket
            .send(Message::Text(serde_json::to_string(&legacy_reply)?))
            .await?;

        state.event_tx.send("legacy-event".into())?;
        assert_eq!(next_text(&mut socket).await?, "legacy-event");

        socket.close(None).await?;
        wait_for_consumers(&state, 0).await?;
        server.abort();
        Ok(())
    }
}

/// Render a channel id for logs, hashing it when it is an ACP channel or session id.
///
/// An ACP `channel_id` is `acp_<uuid>` and the session id is `sess_<same uuid>`, so the two are
/// mutually derivable: either form printed here IS a resume credential. Anyone reading operator logs
/// could resume the session, and logs travel further than the sessions they describe.
///
/// **The uuid is hashed, not the prefixed string.** One session reaches this function as
/// `acp_<uuid>` and elsewhere as `sess_<uuid>`; hashing the whole string gives those two forms a
/// different tag each, and a third different again from this crate's own `redact_id` and
/// `openab-core`'s `redact_session_ids`, which strip the prefix first. Several tags for one session
/// defeat the only purpose the tag has — following that session across logs — more completely than
/// not redacting would.
///
/// Only ACP ids are hashed. A Discord or Slack channel id is a public identifier that operators
/// legitimately grep for, and redacting it would cost real debuggability to protect nothing.
///
/// Copies of this function live in `openab-core` and `openab-mcp` because those crates deliberately
/// do not depend on one another. Each is pinned to the same vector; where a crate has a redactor of
/// its own, its test compares against that rather than against a copied literal.
fn redact_channel(id: &str) -> String {
    let Some(uuid) = id
        .strip_prefix("acp_")
        .or_else(|| id.strip_prefix("sess_"))
        .filter(|uuid| !uuid.is_empty())
    else {
        return id.to_string();
    };
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(uuid.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("#{short}")
}

#[cfg(test)]
mod redact_channel_tests {
    const CHANNEL: &str = "acp_00000000-0000-0000-0000-000000000000";
    const SESSION: &str = "sess_00000000-0000-0000-0000-000000000000";

    /// The tag for a given session must be IDENTICAL in every crate that logs a channel id, and
    /// identical across the two forms one session is addressed by.
    ///
    /// `#12b9377c` is the uuid's tag, shared with `redact_id` below and with `openab-core`'s
    /// `redact_session_ids`. It used to be `#850414fa` here, the hash of the whole `acp_<uuid>`
    /// string, which is why one session could appear under two tags depending on which log you were
    /// reading.
    #[test]
    fn an_acp_id_hashes_its_uuid_to_the_shared_vector_and_others_pass_through() {
        assert_eq!(
            super::redact_channel(CHANNEL),
            "#12b9377c",
            "ACP channel ids must hash to the tag the other crates produce for the same session"
        );
        assert_eq!(
            super::redact_channel(SESSION),
            "#12b9377c",
            "both forms of one session must share a tag — hashing the prefix is what split them"
        );
        assert_eq!(
            super::redact_channel("1234567890"),
            "1234567890",
            "a non-ACP channel id is a public identifier and must stay greppable"
        );
        assert_eq!(
            super::redact_channel("-"),
            "-",
            "the no-session sentinel must not be hashed into something that looks like a session"
        );
    }

    /// Two redactors in ONE crate disagreeing is what produced the split in the first place, so this
    /// compares them directly instead of trusting that both literals were updated together.
    #[cfg(feature = "acp")]
    #[test]
    fn the_channel_tag_matches_this_crates_other_redactor() {
        for id in [CHANNEL, SESSION] {
            assert_eq!(
                super::redact_channel(id),
                crate::adapters::acp_server::redact_id(id),
                "redact_channel and redact_id must tag {id} identically"
            );
        }
    }
}

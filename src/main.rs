mod ctl;
#[cfg(any(
    feature = "telegram",
    feature = "line",
    feature = "feishu",
    feature = "googlechat",
    feature = "wecom",
    feature = "teams",
))]
mod unified_adapter;
use openab_core::acp;
use openab_core::adapter::{self, AdapterRouter};
use openab_core::bot_turns;
use openab_core::config;
use openab_core::cron;
#[cfg(feature = "discord")]
use openab_core::discord;
use openab_core::dispatch;
use openab_core::gateway;
use openab_core::hooks;
use openab_core::multibot_cache;
#[cfg(feature = "discord")]
use openab_core::remind;
use openab_core::secrets;
use openab_core::setup;
#[cfg(feature = "slack")]
use openab_core::slack;

use clap::Parser;
#[cfg(feature = "discord")]
use serenity::gateway::GatewayError;
#[cfg(feature = "discord")]
use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

/// Wait for SIGINT (ctrl_c) or, on unix, SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to install SIGTERM handler, falling back to ctrl_c only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => { info!("SIGTERM received"); }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Parser)]
#[command(name = "openab", version)]
#[command(about = "Multi-platform ACP agent broker (Discord, Slack)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Run the bot (default)
    Run {
        /// Config file path or URL — local path, https://, http://, or s3://<bucket>/<key> (default: config.toml)
        #[arg(short = 'c', long = "config", value_name = "CONFIG")]
        config: Option<String>,
    },
    /// Launch the interactive setup wizard
    Setup {
        /// Output file path for generated config (default: config.toml)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Internal: AgentCore WebSocket shell bridge (ACP↔WebSocket)
    #[cfg(feature = "agentcore")]
    AgentcoreBridge {
        /// AgentCore Runtime ARN
        #[arg(long)]
        runtime_arn: String,
        /// AWS region
        #[arg(long, default_value = "us-east-1")]
        region: String,
        /// ACP agent command to run in the PTY (default: kiro-cli acp --trust-all-tools)
        #[arg(long, default_value = "kiro-cli acp --trust-all-tools")]
        command: String,
    },
    /// Set a runtime value (e.g. thread.name)
    Set {
        /// Key to set (e.g. thread.name)
        key: String,
        /// Value to set
        value: String,
        /// Target thread/channel ID
        #[arg(long)]
        thread: Option<String>,
    },
    /// Get a runtime value
    Get {
        /// Key to get (e.g. thread.name)
        key: String,
        /// Target thread/channel ID
        #[arg(long)]
        thread: Option<String>,
    },
}

/// Returns true if any unified platform env var is set AND the corresponding feature is compiled in.
/// Single source of truth — used by both startup validation and adapter init.
fn has_unified_platform_env() -> bool {
    (cfg!(feature = "telegram") && std::env::var("TELEGRAM_BOT_TOKEN").is_ok())
        || (cfg!(feature = "line") && std::env::var("LINE_CHANNEL_SECRET").is_ok())
        || (cfg!(feature = "feishu") && std::env::var("FEISHU_APP_ID").is_ok())
        || (cfg!(feature = "wecom") && std::env::var("WECOM_CORP_ID").is_ok())
        || (cfg!(feature = "teams") && std::env::var("TEAMS_APP_ID").is_ok())
        || (cfg!(feature = "googlechat")
            && std::env::var("GOOGLE_CHAT_ENABLED")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "openab=info".into()),
        )
        .init();

    let cmd = Cli::parse()
        .command
        .unwrap_or(Commands::Run { config: None });

    let config_arg = match cmd {
        Commands::Setup { output } => {
            setup::run_setup(output.map(PathBuf::from))?;
            return Ok(());
        }
        #[cfg(feature = "agentcore")]
        Commands::AgentcoreBridge {
            runtime_arn,
            region,
            command,
        } => {
            return acp::agentcore::run_bridge(&runtime_arn, &region, &command).await;
        }
        Commands::Set { key, value, thread } => {
            let resp = ctl::send_request(&ctl::Request {
                action: ctl::Action::Set,
                key,
                value: Some(value),
                thread_id: thread.or_else(|| std::env::var("OPENAB_THREAD_ID").ok()),
            })
            .await?;
            if resp.ok {
                println!("✓ {}", resp.message);
            } else {
                eprintln!("✗ {}", resp.message);
                std::process::exit(1);
            }
            return Ok(());
        }
        Commands::Get { key, thread } => {
            let resp = ctl::send_request(&ctl::Request {
                action: ctl::Action::Get,
                key,
                value: None,
                thread_id: thread.or_else(|| std::env::var("OPENAB_THREAD_ID").ok()),
            })
            .await?;
            if resp.ok {
                println!("{}", resp.value.unwrap_or_default());
            } else {
                eprintln!("✗ {}", resp.message);
                std::process::exit(1);
            }
            return Ok(());
        }
        Commands::Run { config } => config,
    };

    // -- Run path --
    let config_source = config_arg.unwrap_or_else(|| "config.toml".into());

    // First pass: load config (env vars expanded, secrets NOT resolved yet)
    let raw_expanded = config::load_config_raw_from_source(&config_source).await?;

    let mut cfg = config::parse_config_str(&raw_expanded, &config_source)?;
    info!(
        agent_cmd = %cfg.agent.command,
        pool_max = cfg.pool.max_sessions,
        discord = cfg.discord.is_some(),
        slack = cfg.slack.is_some(),
        reactions = cfg.reactions.enabled,
        "config loaded"
    );

    if cfg.discord.is_none()
        && cfg.slack.is_none()
        && cfg.gateway.is_none()
        && cfg.telegram.is_none()
        && !has_unified_platform_env()
    {
        anyhow::bail!(
            "no adapter configured — add [discord], [slack], [telegram], or [gateway] to config, or set platform env vars (TELEGRAM_BOT_TOKEN, etc.)"
        );
    }

    // --- Lifecycle hooks: Unix-only. Fail fast on unsupported platforms. ---
    cfg.hooks.ensure_platform_supported()?;

    // --- pre_seed: download & extract S3 zips before pre_boot ---
    #[cfg(feature = "pre-seed")]
    if let Some(ref pre_seed) = cfg.hooks.pre_seed {
        if !pre_seed.sources.is_empty() {
            openab_core::pre_seed::run(pre_seed).await?;
        }
    }

    // Validate and run pre_boot hook (before agent pool creation)
    if let Some(ref hook) = cfg.hooks.pre_boot {
        hooks::validate_hook("pre_boot", hook)?;
        hooks::run_hook("pre_boot", hook).await?;
    }
    if let Some(ref hook) = cfg.hooks.pre_shutdown {
        hooks::validate_hook("pre_shutdown", hook)?;
    }

    // Resolve secrets (after pre_boot hooks so exec:// scripts are available)
    if !cfg.secrets.refs.is_empty() {
        let resolved = secrets::resolve(&cfg.secrets).await?;
        let substituted = secrets::substitute(&raw_expanded, &resolved);
        cfg = config::parse_config_str(&substituted, &config_source)?;
    }

    let shutdown_hook = cfg.hooks.pre_shutdown.clone();

    let pool = Arc::new(acp::SessionPool::new(
        cfg.agent,
        cfg.pool.max_sessions,
        cfg.pool
            .prompt_hard_timeout_secs
            .saturating_add(cfg.pool.hung_grace_secs),
        cfg.pool.default_config_options,
    ));
    let ttl_secs = cfg.pool.session_ttl_hours * 3600;

    // Resolve STT config (auto-detect GROQ_API_KEY from env)
    if cfg.stt.enabled {
        if cfg.stt.api_key.is_empty() && cfg.stt.base_url.contains("groq.com") {
            if let Ok(key) = std::env::var("GROQ_API_KEY") {
                if !key.is_empty() {
                    info!("stt.api_key not set, using GROQ_API_KEY from environment");
                    cfg.stt.api_key = key;
                }
            }
        }
        if cfg.stt.api_key.is_empty() {
            anyhow::bail!("stt.enabled = true but no API key found — set stt.api_key in config or export GROQ_API_KEY");
        }
        info!(model = %cfg.stt.model, base_url = %cfg.stt.base_url, "STT enabled");
    }

    // Build the per-platform trust registry for the gateway platforms from the
    // same GATEWAY_* env the unified bridge uses (behavior-preserving: defaults
    // allow-all, matching today's should_skip_event). L2/L3 enforcement moves to
    // the router's ingress gate; should_skip_event keeps only bot + @mention
    // gating for the unified path. Discord/Slack are wired in a later PR.
    let gateway_trust = {
        use openab_core::trust::{PlatformTrustConfigs, TrustConfig};
        let env_bool = |k: &str, default: bool| {
            std::env::var(k)
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(default)
        };
        let env_set = |k: &str| -> Vec<String> {
            std::env::var(k)
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let allow_all_channels = env_bool("GATEWAY_ALLOW_ALL_CHANNELS", true);
        let allowed_channels = env_set("GATEWAY_ALLOWED_CHANNELS");
        // L3 identity: trust-none by default (Phase 3). Was `true` in #1267
        // (behavior-preserving); now defaults deny-all — set GATEWAY_ALLOW_ALL_USERS=true
        // or list GATEWAY_ALLOWED_USERS to admit senders. L2 (channels) stays open.
        let allow_all_users = env_bool("GATEWAY_ALLOW_ALL_USERS", false);
        let allowed_users = env_set("GATEWAY_ALLOWED_USERS");
        let mut reg = PlatformTrustConfigs::new();
        for platform in ["telegram", "line", "feishu", "wecom", "googlechat", "teams"] {
            reg.insert(
                platform,
                TrustConfig::new(
                    Some(allow_all_channels),
                    allowed_channels.clone(),
                    None, // allow_dm unused in Phase 1 (is_dm passed as false)
                    Some(allow_all_users),
                    allowed_users.clone(),
                ),
            );
        }

        // Discord: gate L3 (identity) only via the shared gate. Discord's L2 is
        // richer than the flat allowed_channels model (threads are admitted by
        // *parent* channel, DMs by allow_dm), so we leave channel/DM enforcement
        // in the adapter and set L2 open here. L3 mirrors the resolved
        // [discord].allow_all_users/allowed_users, so the gate agrees with
        // Discord's existing user check (behavior-preserving). L2 + dispatch-path
        // privatization for Discord follow once the richer channel model lands.
        if let Some(d) = &cfg.discord {
            reg.insert(
                "discord",
                TrustConfig::new(
                    Some(true), // L2 open — Discord's own channel/thread/DM logic still applies
                    Vec::<String>::new(),
                    Some(true),
                    Some(config::resolve_allow_all(
                        d.allow_all_users,
                        &d.allowed_users,
                    )),
                    d.allowed_users.clone(),
                ),
            );
        }

        // Slack: gate L3 (identity) only via the shared gate, mirroring the
        // Discord entry above. Slack's L2 (channel allowlist) stays in the
        // adapter — its registry entry is L2-open — and L3 mirrors the resolved
        // [slack].allow_all_users/allowed_users, so the gate agrees with
        // Slack's existing user check (behavior-preserving). Without this
        // entry, Slack was the only configured platform absent from the
        // registry, falling back to the deny-all default if the gate ever ran
        // for it (#1361).
        if let Some(s) = &cfg.slack {
            reg.insert(
                "slack",
                TrustConfig::new(
                    Some(true), // L2 open — Slack's own channel check still applies
                    Vec::<String>::new(),
                    Some(true),
                    Some(config::resolve_allow_all(
                        s.allow_all_users,
                        &s.allowed_users,
                    )),
                    s.allowed_users.clone(),
                ),
            );
        }

        // Telegram: L3 (identity) mirrors the resolved
        // [telegram].allow_all_users/allowed_users, so config.toml can
        // restrict who can message the bot without needing
        // GATEWAY_ALLOW_ALL_USERS/GATEWAY_ALLOWED_USERS env vars. L2
        // (channels) has no Telegram-specific concept distinct from the
        // generic gateway model, so it stays on the shared GATEWAY_* values
        // set above.
        //
        // Also resolves when running env-only (no [telegram] section but
        // TELEGRAM_BOT_TOKEN set), so TELEGRAM_ALLOWED_USERS /
        // TELEGRAM_ALLOW_ALL_USERS are honored in pure-env deployments.
        let telegram_resolved = if let Some(t) = &cfg.telegram {
            Some(t.resolve())
        } else if std::env::var("TELEGRAM_ALLOWED_USERS").is_ok()
            || std::env::var("TELEGRAM_ALLOW_ALL_USERS").is_ok()
        {
            Some(config::TelegramConfig::default().resolve())
        } else {
            None
        };
        if let Some(r) = telegram_resolved {
            reg.insert(
                "telegram",
                TrustConfig::new(
                    Some(allow_all_channels),
                    allowed_channels.clone(),
                    None,
                    Some(r.allow_all_users),
                    r.allowed_users,
                ),
            );
        }

        // LINE: L3 (identity) mirrors the resolved
        // [line].allow_all_users/allowed_users, so config.toml can restrict
        // who can message the bot without the uniform
        // GATEWAY_ALLOW_ALL_USERS/GATEWAY_ALLOWED_USERS env vars (#1355). L2
        // (channels) has no LINE-specific concept distinct from the generic
        // gateway model yet (group policy is a follow-up), so it stays on the
        // shared GATEWAY_* values set above.
        //
        // Also resolves when running env-only (no [line] section but
        // LINE_ALLOW_ALL_USERS / LINE_ALLOWED_USERS set), matching the
        // Telegram pattern.
        let line_resolved = if let Some(l) = &cfg.line {
            Some(l.resolve())
        } else if config::LineConfig::env_trust_present() {
            Some(config::LineConfig::default().resolve())
        } else {
            None
        };
        match line_resolved {
            Some(r) => {
                reg.insert(
                    "line",
                    TrustConfig::new(
                        Some(allow_all_channels),
                        allowed_channels.clone(),
                        None,
                        Some(r.allow_all_users),
                        r.allowed_users,
                    ),
                );
            }
            None => {
                // Phase 1 deprecation (#1355/#1356): LINE trust still rides on
                // the uniform GATEWAY_* seed. Warn when the unified LINE
                // adapter is active and the legacy env is what admits users,
                // so operators migrate before Phase 2 turns this into an error.
                let line_active =
                    cfg!(feature = "line") && std::env::var("LINE_CHANNEL_SECRET").is_ok();
                let legacy_env_set = std::env::var("GATEWAY_ALLOW_ALL_USERS").is_ok()
                    || std::env::var("GATEWAY_ALLOWED_USERS").is_ok();
                if line_active && legacy_env_set {
                    warn!(
                        "LINE trust is driven by deprecated GATEWAY_ALLOW_ALL_USERS/\
                         GATEWAY_ALLOWED_USERS env vars — migrate to a [line] section \
                         (allow_all_users/allowed_users) or LINE_ALLOW_ALL_USERS/\
                         LINE_ALLOWED_USERS; the uniform GATEWAY_* fallback will \
                         become a startup error in Phase 2 (#1356)"
                    );
                }
            }
        }
        reg
    };

    let router = Arc::new(
        AdapterRouter::new(
            pool.clone(),
            cfg.reactions,
            cfg.markdown.tables,
            cfg.pool.prompt_hard_timeout_secs,
            cfg.pool.liveness_check_secs,
            cfg.workspace.aliases,
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| {
                tracing::warn!(
                    "HOME environment variable is not set — falling back to /tmp as bot_home. \
                     This weakens the workspace security boundary."
                );
                "/tmp".into()
            })),
        )
        .with_trust(gateway_trust),
    );

    // Shutdown signal for Slack adapter
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let dispatchers: Arc<Mutex<Vec<Arc<dispatch::Dispatcher>>>> = Arc::new(Mutex::new(Vec::new()));

    // Spawn cleanup task
    let cleanup_pool = pool.clone();
    let cleanup_dispatchers = dispatchers.clone();
    let cleanup_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cleanup_pool.cleanup_idle(ttl_secs).await;
            for d in cleanup_dispatchers.lock().unwrap().iter() {
                d.sweep_stale();
            }
        }
    });

    // Pre-build shared adapters for cron scheduler
    #[cfg(feature = "discord")]
    let shared_discord_adapter: Option<Arc<dyn adapter::ChatAdapter>> =
        cfg.discord.as_ref().map(|dc| {
            let http = Arc::new(serenity::http::Http::new(&dc.bot_token));
            Arc::new(discord::DiscordAdapter::new(http)) as Arc<dyn adapter::ChatAdapter>
        });
    #[cfg(not(feature = "discord"))]
    let shared_discord_adapter: Option<Arc<dyn adapter::ChatAdapter>> = None;

    let session_ttl_dur = std::time::Duration::from_secs(ttl_secs);

    // Initialize multibot cache
    let multibot_cache_path = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".openab")
        .join("cache")
        .join("threads.json");
    let multibot_cache = multibot_cache::MultibotCache::load(multibot_cache_path);

    // Initialize filestore (for uploading file attachments to S3/R2).
    #[cfg(feature = "filestore")]
    let filestore: Option<Arc<openab_core::filestore::Filestore>> = if let Some(ref fs_cfg) =
        cfg.filestore
    {
        info!(
            bucket = %fs_cfg.bucket,
            region = %fs_cfg.region,
            prefix = %fs_cfg.prefix,
            presigned_ttl = fs_cfg.presigned_ttl,
            "filestore enabled"
        );
        Some(Arc::new(
            openab_core::filestore::Filestore::new(fs_cfg).await,
        ))
    } else {
        None
    };

    #[cfg(feature = "slack")]
    let shared_slack_adapter: Option<Arc<slack::SlackAdapter>> = cfg.slack.as_ref().map(|s| {
        Arc::new(slack::SlackAdapter::new(
            s.bot_token.clone(),
            session_ttl_dur,
            s.allow_bot_messages,
            s.assistant_mode,
            multibot_cache.clone(),
            s.streaming,
        ))
    });
    #[cfg(not(feature = "slack"))]
    let shared_slack_adapter: Option<Arc<dyn adapter::ChatAdapter>> = None;

    // Shared slot for Discord ShardMessenger (set in ready handler, used by ctl for agent.status)
    #[cfg(unix)]
    let ctl_shard: ctl::ShardSlot = Arc::new(std::sync::OnceLock::new());

    // Thread registry: thread_id → platform. Populated on message dispatch.
    #[cfg(unix)]
    let ctl_registry = ctl::new_registry();

    // Spawn control socket server for `openab set/get` IPC
    #[cfg(unix)]
    let ctl_handle = {
        let mut adapters = std::collections::HashMap::new();
        if let Some(ref a) = shared_discord_adapter {
            adapters.insert("discord".into(), a.clone());
        }
        if let Some(ref a) = shared_slack_adapter {
            adapters.insert("slack".into(), a.clone() as Arc<dyn adapter::ChatAdapter>);
        }
        if adapters.is_empty() {
            None
        } else {
            Some(ctl::spawn_server(Arc::new(ctl::RuntimeHandler::new(
                adapters,
                ctl_registry.clone(),
                ctl_shard.clone(),
            ))))
        }
    };
    #[cfg(not(unix))]
    let ctl_handle: Option<tokio::task::JoinHandle<()>> = None;

    // Validate cronjob config at startup
    let mut configured_platforms: Vec<&str> = Vec::new();
    if cfg.discord.is_some() {
        configured_platforms.push("discord");
    }
    if cfg.slack.is_some() {
        configured_platforms.push("slack");
    }
    #[cfg(feature = "telegram")]
    if cfg.telegram.is_some() || std::env::var("TELEGRAM_BOT_TOKEN").is_ok() {
        configured_platforms.push("telegram");
    }
    cron::validate_cronjobs(&cfg.cron.jobs, &configured_platforms)?;

    // Spawn Slack adapter (background task)
    #[cfg(feature = "slack")]
    let slack_handle = if let Some(slack_cfg) = cfg.slack {
        let allow_all_channels =
            config::resolve_allow_all(slack_cfg.allow_all_channels, &slack_cfg.allowed_channels);
        let allow_all_users =
            config::resolve_allow_all(slack_cfg.allow_all_users, &slack_cfg.allowed_users);
        if !allow_all_channels && slack_cfg.allowed_channels.is_empty() {
            warn!("allow_all_channels=false with empty allowed_channels for Slack — bot will deny all channels");
        }
        info!(
            allow_all_channels,
            allow_all_users,
            channels = slack_cfg.allowed_channels.len(),
            users = slack_cfg.allowed_users.len(),
            allow_bot_messages = ?slack_cfg.allow_bot_messages,
            allow_user_messages = ?slack_cfg.allow_user_messages,
            "starting slack adapter"
        );
        let router = router.clone();
        let stt = cfg.stt.clone();
        let max_bot_turns = slack_cfg.max_bot_turns;
        let slack_shutdown_rx = shutdown_rx.clone();
        let adapter = shared_slack_adapter
            .clone()
            .expect("shared_slack_adapter must exist when slack config is present");
        let (slack_cap, slack_grouping, slack_idle) = dispatch::dispatch_params(
            &slack_cfg.message_processing_mode,
            slack_cfg.max_buffered_messages,
        );
        let slack_dispatcher = Arc::new(dispatch::Dispatcher::with_idle_timeout(
            router.clone(),
            slack_cap,
            slack_cfg.max_batch_tokens,
            slack_grouping,
            slack_idle,
        ));
        dispatchers.lock().unwrap().push(slack_dispatcher.clone());
        let slack_allowed_users: std::collections::HashSet<String> =
            slack_cfg.allowed_users.into_iter().collect();
        let slack_router = router.clone();
        #[cfg(feature = "filestore")]
        let slack_filestore = filestore.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = slack::run_slack_adapter(
                adapter,
                slack_router,
                slack_cfg.app_token,
                allow_all_channels,
                allow_all_users,
                slack_cfg.allowed_channels.into_iter().collect(),
                slack_allowed_users,
                slack_cfg.allow_bot_messages,
                slack_cfg.trusted_bot_ids.into_iter().collect(),
                slack_cfg.allow_user_messages,
                max_bot_turns,
                stt,
                slack_shutdown_rx,
                slack_dispatcher,
                #[cfg(feature = "filestore")]
                slack_filestore,
            )
            .await
            {
                error!("slack adapter error: {e}");
            }
        }))
    } else {
        None
    };
    #[cfg(not(feature = "slack"))]
    let slack_handle: Option<tokio::task::JoinHandle<()>> = None;

    // Spawn Gateway adapter (background task)
    let gateway_handle = if let Some(gw_cfg) = cfg.gateway {
        let router = router.clone();
        let shutdown_rx = shutdown_rx.clone();
        info!(url = %gw_cfg.url, "starting gateway adapter");
        let (gw_cap, gw_grouping, gw_idle) = dispatch::dispatch_params(
            &gw_cfg.message_processing_mode,
            gw_cfg.max_buffered_messages,
        );
        let gw_dispatcher = Arc::new(dispatch::Dispatcher::with_idle_timeout(
            router.clone(),
            gw_cap,
            gw_cfg.max_batch_tokens,
            gw_grouping,
            gw_idle,
        ));
        dispatchers.lock().unwrap().push(gw_dispatcher.clone());
        let params = gateway::GatewayParams {
            url: gw_cfg.url,
            platform: gw_cfg.platform,
            token: gw_cfg.token,
            bot_username: gw_cfg.bot_username,
            allow_all_channels: config::resolve_allow_all(
                gw_cfg.allow_all_channels,
                &gw_cfg.allowed_channels,
            ),
            allowed_channels: gw_cfg.allowed_channels,
            allow_all_users: config::resolve_allow_all(
                gw_cfg.allow_all_users,
                &gw_cfg.allowed_users,
            ),
            allowed_users: gw_cfg.allowed_users,
            allow_bot_messages: gw_cfg.allow_bot_messages,
            trusted_bot_ids: gw_cfg.trusted_bot_ids,
            streaming: gw_cfg.streaming,
            streaming_placeholder: gw_cfg.streaming_placeholder,
            telegram_rich_messages: gw_cfg.telegram_rich_messages,
            stt: cfg.stt.clone(),
        };
        let gw_router = router.clone();
        #[cfg(feature = "filestore")]
        let gw_filestore = filestore.clone();
        Some(tokio::spawn(async move {
            if let Err(e) =
                gateway::run_gateway_adapter(
                    params,
                    shutdown_rx,
                    gw_dispatcher,
                    gw_router,
                    #[cfg(feature = "filestore")]
                    gw_filestore,
                ).await
            {
                error!("gateway adapter error: {e}");
            }
        }))
    } else {
        None
    };

    // Spawn cron scheduler (background task)
    // Spawn embedded webhook server when gateway adapters are compiled in (unified mode).
    // In unified mode, platform webhooks hit this axum server directly → Dispatcher.submit(),
    // bypassing the WebSocket hop of the two-process model.
    #[cfg(any(
        feature = "telegram",
        feature = "line",
        feature = "feishu",
        feature = "googlechat",
        feature = "wecom",
        feature = "teams",
    ))]
    let (_unified_handle, shared_unified_adapter) = {
        use openab_core::gateway::{process_gateway_event, GatewayEventContext};

        if has_unified_platform_env() || cfg.telegram.is_some() {
            let listen_addr =
                std::env::var("GATEWAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());

            // Create a dedicated dispatcher for unified gateway events
            let unified_dispatcher = Arc::new(dispatch::Dispatcher::with_idle_timeout(
                router.clone(),
                1,
                24_000,
                dispatch::BatchGrouping::Thread,
                dispatch::PER_MESSAGE_CONSUMER_IDLE_TIMEOUT,
            ));
            dispatchers.lock().unwrap().push(unified_dispatcher.clone());

            // Bridge: reuse gateway crate's AppState + webhook handlers.
            // Events flow: webhook → adapter handler → event_tx → bridge task → process_gateway_event() → Dispatcher
            // This reuses 100% of existing adapter code (signature verify, parsing, etc).
            let (event_tx, _) = tokio::sync::broadcast::channel::<String>(256);

            // Build gateway AppState from env vars (shared factory with standalone gateway)
            let mut gw_state_inner = openab_gateway::AppState::from_env(event_tx.clone(), None);


            // First-class `[telegram]` config overrides env-derived values
            // (config-authoritative + ${} expansion + TELEGRAM_* env fallback).
            #[cfg_attr(not(feature = "telegram"), allow(unused_variables))]
            let telegram_webhook_path = if let Some(ref tg) = cfg.telegram {
                let r = tg.resolve();
                let path = r.webhook_path.clone();
                gw_state_inner.apply_telegram_config(openab_gateway::GatewayTelegramConfig {
                    bot_token: r.bot_token,
                    secret_token: r.secret_token,
                    rich_messages: r.rich_messages,
                    trusted_source_only: r.trusted_source_only,
                    streaming: r.streaming,
                });
                Some(path)
            } else {
                None
            };
            let gw_state = Arc::new(gw_state_inner);

            // Build axum router with platform webhook routes
            let mut app =
                axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));

            #[cfg(feature = "telegram")]
            if gw_state.telegram_bot_token.is_some() {
                let path = telegram_webhook_path.clone().unwrap_or_else(|| {
                    std::env::var("TELEGRAM_WEBHOOK_PATH")
                        .unwrap_or_else(|_| "/webhook/telegram".into())
                });
                info!(path = %path, "unified: telegram adapter enabled");
                app = app.route(
                    &path,
                    axum::routing::post(openab_gateway::adapters::telegram::webhook),
                );
            }

            #[cfg(feature = "line")]
            {
                info!("unified: line adapter enabled");
                app = app.route(
                    "/webhook/line",
                    axum::routing::post(openab_gateway::adapters::line::webhook),
                );
            }

            #[cfg(feature = "feishu")]
            if gw_state.feishu.is_some() {
                let path = std::env::var("FEISHU_WEBHOOK_PATH")
                    .unwrap_or_else(|_| "/webhook/feishu".into());
                info!(path = %path, "unified: feishu adapter enabled");
                app = app.route(
                    &path,
                    axum::routing::post(openab_gateway::adapters::feishu::webhook),
                );
            }

            #[cfg(feature = "wecom")]
            if let Some(ref w) = gw_state.wecom {
                info!(path = %w.config.webhook_path, "unified: wecom adapter enabled");
                app = app
                    .route(
                        &w.config.webhook_path,
                        axum::routing::get(openab_gateway::adapters::wecom::verify),
                    )
                    .route(
                        &w.config.webhook_path,
                        axum::routing::post(openab_gateway::adapters::wecom::webhook),
                    );
            }

            #[cfg(feature = "teams")]
            if gw_state.teams.is_some() {
                let path =
                    std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());
                info!(path = %path, "unified: teams adapter enabled");
                app = app.route(
                    &path,
                    axum::routing::post(openab_gateway::adapters::teams::webhook),
                );
            }

            #[cfg(feature = "googlechat")]
            if gw_state.google_chat.is_some() {
                let path = std::env::var("GOOGLE_CHAT_WEBHOOK_PATH")
                    .unwrap_or_else(|_| "/webhook/googlechat".into());
                info!(path = %path, "unified: googlechat adapter enabled");
                app = app.route(
                    &path,
                    axum::routing::post(openab_gateway::adapters::googlechat::webhook),
                );
            }

            let app = app.with_state(gw_state.clone());

            // Bridge task: receive events from adapters via event_tx, dispatch to core
            let unified_adapter: Arc<dyn adapter::ChatAdapter> = Arc::new(
                unified_adapter::UnifiedGatewayAdapter::new(gw_state.clone()),
            );

            // Read security gating from env (mirrors [gateway] config section)
            let gw_allow_all_channels = std::env::var("GATEWAY_ALLOW_ALL_CHANNELS")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let gw_allowed_channels: std::collections::HashSet<String> =
                std::env::var("GATEWAY_ALLOWED_CHANNELS")
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            let gw_allow_all_users = std::env::var("GATEWAY_ALLOW_ALL_USERS")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(true);
            let gw_allowed_users: std::collections::HashSet<String> =
                std::env::var("GATEWAY_ALLOWED_USERS")
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            let gw_bot_username = std::env::var("GATEWAY_BOT_USERNAME").ok();

            let gw_allow_bot_messages = std::env::var("GATEWAY_ALLOW_BOT_MESSAGES")
                .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
                .unwrap_or(false);
            let gw_trusted_bot_ids: std::collections::HashSet<String> =
                std::env::var("GATEWAY_TRUSTED_BOT_IDS")
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

            let cron_unified_adapter = unified_adapter.clone();

            let event_ctx = Arc::new(GatewayEventContext {
                adapter: unified_adapter,
                dispatcher: unified_dispatcher,
                router: router.clone(),
                allow_all_channels: config::resolve_allow_all(
                    Some(gw_allow_all_channels),
                    &gw_allowed_channels.iter().cloned().collect::<Vec<_>>(),
                ),
                allowed_channels: gw_allowed_channels,
                allow_all_users: config::resolve_allow_all(
                    Some(gw_allow_all_users),
                    &gw_allowed_users.iter().cloned().collect::<Vec<_>>(),
                ),
                allowed_users: gw_allowed_users,
                allow_bot_messages: gw_allow_bot_messages,
                trusted_bot_ids: gw_trusted_bot_ids,
                bot_username: gw_bot_username,
                stt_config: cfg.stt.clone(),
                #[cfg(feature = "filestore")]
                filestore: filestore.clone(),
            });

            // Spawn the event bridge (event_tx → process_gateway_event)
            let mut event_rx = event_tx.subscribe();
            let bridge_ctx = event_ctx.clone();
            tokio::spawn(async move {
                loop {
                    match event_rx.recv().await {
                        Ok(event_json) => {
                            let ctx = bridge_ctx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = process_gateway_event(&event_json, &ctx).await {
                                    tracing::warn!(error = %e, "unified bridge: event processing failed");
                                }
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "unified bridge: event_rx lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            info!(addr = %listen_addr, "unified webhook server starting");

            (Some(tokio::spawn(async move {
                let listener = match tokio::net::TcpListener::bind(&listen_addr).await {
                    Ok(l) => l,
                    Err(e) => {
                        error!(addr = %listen_addr, error = %e, "unified webhook server bind failed");
                        return;
                    }
                };
                info!(addr = %listen_addr, "unified webhook server listening");
                if let Err(e) = axum::serve(listener, app).await {
                    error!(error = %e, "unified webhook server error");
                }
            })), Some(cron_unified_adapter))
        } else {
            (None, None)
        }
    };

    let usercron_path = if cfg.cron.usercron_enabled {
        cfg.cron.usercron_path.as_ref().map(|p| {
            let path = std::path::PathBuf::from(p);
            if path.is_absolute() {
                path
            } else {
                std::env::var("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join(".openab")
                    .join(path)
            }
        })
    } else {
        None
    };
    let has_cron_work = !cfg.cron.jobs.is_empty() || usercron_path.is_some();
    let cron_handle = if has_cron_work {
        let shutdown_rx = shutdown_rx.clone();
        let cronjobs = cfg.cron.jobs.clone();
        let cron_router = router.clone();
        let mut cron_adapters: std::collections::HashMap<String, Arc<dyn adapter::ChatAdapter>> =
            std::collections::HashMap::new();
        if let Some(ref a) = shared_discord_adapter {
            cron_adapters.insert("discord".into(), a.clone());
        }
        #[cfg(feature = "slack")]
        if let Some(ref a) = shared_slack_adapter {
            cron_adapters.insert("slack".into(), a.clone() as Arc<dyn adapter::ChatAdapter>);
        }
        #[cfg(feature = "telegram")]
        if let Some(ref a) = shared_unified_adapter {
            cron_adapters.insert("telegram".into(), a.clone());
        }
        let cron_platforms: Vec<String> =
            configured_platforms.iter().map(|s| s.to_string()).collect();
        info!(baseline = cronjobs.len(), usercron = ?usercron_path, "starting cron scheduler");
        Some(tokio::spawn(async move {
            cron::run_scheduler(
                cronjobs,
                usercron_path,
                cron_platforms,
                cron_router,
                cron_adapters,
                shutdown_rx,
            )
            .await;
        }))
    } else {
        None
    };

    // Run Discord adapter (foreground, blocking) or wait for ctrl_c
    #[cfg(feature = "discord")]
    if let Some(discord_cfg) = cfg.discord {
        let allow_all_channels = config::resolve_allow_all(
            discord_cfg.allow_all_channels,
            &discord_cfg.allowed_channels,
        );
        let allow_all_users =
            config::resolve_allow_all(discord_cfg.allow_all_users, &discord_cfg.allowed_users);
        let allowed_channels =
            parse_id_set(&discord_cfg.allowed_channels, "discord.allowed_channels")?;
        if !allow_all_channels && allowed_channels.is_empty() {
            warn!("allow_all_channels=false with empty allowed_channels for Discord — bot will deny all channels");
        }
        let allowed_users = parse_id_set(&discord_cfg.allowed_users, "discord.allowed_users")?;
        let trusted_bot_ids =
            parse_id_set(&discord_cfg.trusted_bot_ids, "discord.trusted_bot_ids")?;
        let allowed_role_ids =
            parse_id_set(&discord_cfg.allowed_role_ids, "discord.allowed_role_ids")?;
        info!(
            allow_all_channels,
            allow_all_users,
            channels = allowed_channels.len(),
            users = allowed_users.len(),
            trusted_bots = trusted_bot_ids.len(),
            role_triggers = allowed_role_ids.len(),
            allow_bot_messages = ?discord_cfg.allow_bot_messages,
            allow_user_messages = ?discord_cfg.allow_user_messages,
            allow_dm = discord_cfg.allow_dm,
            "starting discord adapter"
        );

        let (discord_cap, discord_grouping, discord_idle) = dispatch::dispatch_params(
            &discord_cfg.message_processing_mode,
            discord_cfg.max_buffered_messages,
        );
        let discord_dispatcher = Arc::new(dispatch::Dispatcher::with_idle_timeout(
            router.clone(),
            discord_cap,
            discord_cfg.max_batch_tokens,
            discord_grouping,
            discord_idle,
        ));
        dispatchers.lock().unwrap().push(discord_dispatcher.clone());

        // Initialize reminder store
        let reminder_path = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join(".openab")
            .join("reminders.json");
        let reminder_store = remind::ReminderStore::load(reminder_path);

        // Construct ambient dispatcher if enabled and channels configured.
        let ambient_dispatcher = if cfg.ambient.enabled && !cfg.ambient.discord.channels.is_empty()
        {
            info!(
                channels = ?cfg.ambient.discord.channels,
                flush_interval = cfg.ambient.flush_interval_seconds,
                flush_max_messages = cfg.ambient.flush_max_messages,
                "ambient mode enabled"
            );
            Some(Arc::new(openab_core::ambient::AmbientDispatcher::new(
                cfg.ambient.clone(),
            )))
        } else {
            None
        };

        let handler = discord::Handler {
            router,
            allow_all_channels,
            allow_all_users,
            allowed_channels,
            allowed_users,
            stt_config: cfg.stt.clone(),
            adapter: std::sync::OnceLock::new(),
            #[cfg(feature = "filestore")]
            filestore: filestore.clone(),
            allow_bot_messages: discord_cfg.allow_bot_messages,
            trusted_bot_ids,
            allow_user_messages: discord_cfg.allow_user_messages,
            allowed_role_ids,
            participated_threads: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            multibot_threads: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            multibot_cache,
            session_ttl: std::time::Duration::from_secs(ttl_secs),
            max_bot_turns: discord_cfg.max_bot_turns,
            bot_turns: tokio::sync::Mutex::new(bot_turns::BotTurnTracker::new(
                discord_cfg.max_bot_turns,
            )),
            allow_dm: discord_cfg.allow_dm,
            dispatcher: discord_dispatcher,
            ambient: ambient_dispatcher,
            reminder_store: reminder_store.clone(),
            scheduled_ids: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        };

        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::GUILD_MESSAGE_REACTIONS;

        let mut client = Client::builder(&discord_cfg.bot_token, intents)
            .event_handler(handler)
            .await?;

        let shard_manager = client.shard_manager.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            info!("shutdown signal received");
            shard_manager.shutdown_all().await;
        });

        info!("discord bot running");
        match client.start().await {
            Err(serenity::Error::Gateway(GatewayError::DisallowedGatewayIntents)) => {
                error!(
                    "Discord rejected privileged intents. \
                     Enable MESSAGE CONTENT INTENT at: \
                     https://discord.com/developers/applications → Bot → Privileged Gateway Intents"
                );
                std::process::exit(1);
            }
            Err(serenity::Error::Gateway(GatewayError::InvalidAuthentication)) => {
                error!(
                    "Discord rejected bot token. \
                     Verify your bot_token in config.toml is correct and has not been reset."
                );
                std::process::exit(1);
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
    } else {
        info!("running without discord, press ctrl+c to stop");
        shutdown_signal().await;
        info!("shutdown signal received");
    }
    // When discord feature is disabled at compile time, use this fallback block.
    // (When discord feature IS enabled but no [discord] config exists, the `else`
    // branch of the `if let Some(discord_cfg)` above handles shutdown instead.)
    #[cfg(not(feature = "discord"))]
    {
        info!("running without discord, press ctrl+c to stop");
        shutdown_signal().await;
        info!("shutdown signal received");
    }

    // Cleanup
    cleanup_handle.abort();
    if let Some(h) = ctl_handle {
        h.abort();
        let _ = std::fs::remove_file(ctl::socket_path());
    }
    let _ = shutdown_tx.send(true);
    if let Some(handle) = slack_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }
    if let Some(handle) = gateway_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }
    if let Some(handle) = cron_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(35), handle).await;
    }
    for d in dispatchers.lock().unwrap().iter() {
        d.shutdown();
    }
    let shutdown_pool = pool;
    shutdown_pool.shutdown().await;
    if let Some(ref hook) = shutdown_hook {
        if let Err(e) = hooks::run_hook("pre_shutdown", hook).await {
            error!(error = %e, "pre_shutdown hook failed");
        }
    }
    info!("openab shut down");
    Ok(())
}

fn parse_id_set(raw: &[String], label: &str) -> anyhow::Result<HashSet<u64>> {
    let set: HashSet<u64> = raw
        .iter()
        .filter_map(|s| match s.parse() {
            Ok(id) => Some(id),
            Err(_) => {
                tracing::warn!(value = %s, label = label, "ignoring invalid entry");
                None
            }
        })
        .collect();
    if !raw.is_empty() && set.is_empty() {
        anyhow::bail!(
            "all {label} entries failed to parse — refusing to start with an empty allowlist"
        );
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_no_args_defaults_to_run() {
        let cli = Cli::try_parse_from(["openab"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_run_no_args_defaults_config() {
        let cli = Cli::try_parse_from(["openab", "run"]).unwrap();
        match cli.command.unwrap() {
            Commands::Run { config } => assert!(config.is_none()),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn cli_run_with_short_flag_local() {
        let cli = Cli::try_parse_from(["openab", "run", "-c", "my-config.toml"]).unwrap();
        match cli.command.unwrap() {
            Commands::Run { config } => assert_eq!(config.unwrap(), "my-config.toml"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn cli_run_with_long_flag_local() {
        let cli = Cli::try_parse_from(["openab", "run", "--config", "my-config.toml"]).unwrap();
        match cli.command.unwrap() {
            Commands::Run { config } => assert_eq!(config.unwrap(), "my-config.toml"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn cli_run_with_remote_url() {
        let cli = Cli::try_parse_from(["openab", "run", "-c", "https://example.com/config.toml"])
            .unwrap();
        match cli.command.unwrap() {
            Commands::Run { config } => assert!(config.unwrap().starts_with("https://")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn cli_setup_subcommand() {
        let cli = Cli::try_parse_from(["openab", "setup"]).unwrap();
        assert!(matches!(cli.command.unwrap(), Commands::Setup { .. }));
    }

    #[test]
    fn has_unified_platform_env_checks() {
        // Run sequentially in one test to avoid env var race conditions
        // (std::env::set_var is process-global, cargo tests run in parallel)

        // Helper to clear all platform env vars
        fn clear_all() {
            std::env::remove_var("TELEGRAM_BOT_TOKEN");
            std::env::remove_var("LINE_CHANNEL_SECRET");
            std::env::remove_var("FEISHU_APP_ID");
            std::env::remove_var("WECOM_CORP_ID");
            std::env::remove_var("TEAMS_APP_ID");
            std::env::remove_var("GOOGLE_CHAT_ENABLED");
        }

        // Case 1: no env vars → false
        clear_all();
        assert!(!has_unified_platform_env());

        // Case 2: GOOGLE_CHAT_ENABLED=true → true only if feature compiled
        clear_all();
        std::env::set_var("GOOGLE_CHAT_ENABLED", "true");
        assert_eq!(has_unified_platform_env(), cfg!(feature = "googlechat"));

        // Case 3: GOOGLE_CHAT_ENABLED=yes (invalid) → false
        clear_all();
        std::env::set_var("GOOGLE_CHAT_ENABLED", "yes");
        assert!(!has_unified_platform_env());

        // Case 4: TELEGRAM_BOT_TOKEN → true only if feature compiled
        clear_all();
        std::env::set_var("TELEGRAM_BOT_TOKEN", "test-token");
        assert_eq!(has_unified_platform_env(), cfg!(feature = "telegram"));

        // Cleanup
        clear_all();
    }
}

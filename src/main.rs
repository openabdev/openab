mod acp;
mod adapter;
mod bot_turns;
mod config;
mod cron;
mod discord;
mod dispatch;
mod error_display;
mod format;
mod gateway;
mod markdown;
mod media;
mod reactions;
mod setup;
mod slack;
mod stt;
mod timestamp;

use adapter::AdapterRouter;
use clap::Parser;
use serenity::gateway::GatewayError;
use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{error, info, warn};

/// Wait for SIGINT (ctrl_c) or, on unix, SIGTERM. SIGTERM is what Kubernetes
/// sends during pod termination, so handling it lets us run the full cleanup
/// path (shard manager, ACP pool drain) instead of getting SIGKILL'd after the
/// grace period.
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
        /// Config file path or URL (default: config.toml)
        #[arg(short = 'c', long = "config", value_name = "CONFIG")]
        config: Option<String>,
    },
    /// Launch the interactive setup wizard
    Setup {
        /// Output file path for generated config (default: config.toml)
        #[arg(short, long)]
        output: Option<String>,
    },
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
        Commands::Run { config } => config,
    };

    // -- Run path --
    let config_source = config_arg.unwrap_or_else(|| "config.toml".into());

    let mut cfg = if config_source.starts_with("https://") {
        info!(url = %config_source, "fetching remote config");
        config::load_config_from_url(&config_source).await?
    } else if config_source.starts_with("http://") {
        warn!(url = %config_source, "fetching remote config over plaintext HTTP — use HTTPS in production");
        config::load_config_from_url(&config_source).await?
    } else {
        config::load_config(&PathBuf::from(&config_source))?
    };
    info!(
        agent_cmd = %cfg.agent.command,
        pool_max = cfg.pool.max_sessions,
        discord = cfg.discord.is_some(),
        slack = cfg.slack.is_some(),
        reactions = cfg.reactions.enabled,
        "config loaded"
    );

    if cfg.discord.is_none() && cfg.slack.is_none() && cfg.gateway.is_none() {
        anyhow::bail!(
            "no adapter configured — add [discord], [slack], and/or [gateway] to config.toml"
        );
    }

    let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));
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

    let router = Arc::new(AdapterRouter::new(
        pool.clone(),
        cfg.reactions,
        cfg.markdown.tables,
    ));

    // Shutdown signal for Slack adapter
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Dispatcher handles tracked here so SIGTERM cleanup can call shutdown() on each (ADR §6.8).
    // Also shared with the cleanup task for periodic stale-entry sweeping.
    // Arc<Mutex<Vec<…>>> because: outer Arc shared with cleanup task + shutdown,
    // Mutex guards startup-time pushes, inner Arc<Dispatcher> shared with each adapter.
    // All pushes happen at startup; runtime access is read-only (lock is uncontended).
    let dispatchers: Arc<Mutex<Vec<Arc<dispatch::Dispatcher>>>> = Arc::new(Mutex::new(Vec::new()));

    // Spawn cleanup task
    let cleanup_pool = pool.clone();
    let cleanup_dispatchers = dispatchers.clone();
    let cleanup_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cleanup_pool.cleanup_idle(ttl_secs).await;
            // Sweep stale per-thread dispatcher entries (idle-exited consumers).
            for d in cleanup_dispatchers.lock().unwrap().iter() {
                d.sweep_stale();
            }
        }
    });

    // Pre-build shared adapters for cron scheduler (avoids duplicate Http clients / rate-limit buckets)
    let shared_discord_adapter: Option<Arc<dyn adapter::ChatAdapter>> =
        cfg.discord.as_ref().map(|dc| {
            let http = Arc::new(serenity::http::Http::new(&dc.bot_token));
            Arc::new(discord::DiscordAdapter::new(http)) as Arc<dyn adapter::ChatAdapter>
        });
    let session_ttl_dur = std::time::Duration::from_secs(ttl_secs);
    let shared_slack_adapter: Option<Arc<slack::SlackAdapter>> = cfg.slack.as_ref().map(|s| {
        Arc::new(slack::SlackAdapter::new(
            s.bot_token.clone(),
            session_ttl_dur,
            s.allow_bot_messages,
        ))
    });

    // Validate cronjob config at startup (fail-fast on bad cron expressions or timezones)
    let mut configured_platforms: Vec<&str> = Vec::new();
    if cfg.discord.is_some() {
        configured_platforms.push("discord");
    }
    if cfg.slack.is_some() {
        configured_platforms.push("slack");
    }
    cron::validate_cronjobs(&cfg.cron.jobs, &configured_platforms)?;

    // Spawn Slack adapter (background task)
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
        // Dispatcher is the sole serialization path for all modes. Message = cap 1
        // (each message dispatches alone, FIFO). Thread / Lane = configured cap;
        // grouping decides whether senders share a buffer or get their own lane.
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
        Some(tokio::spawn(async move {
            if let Err(e) = slack::run_slack_adapter(
                adapter,
                slack_cfg.app_token,
                allow_all_channels,
                allow_all_users,
                slack_cfg.allowed_channels.into_iter().collect(),
                slack_cfg.allowed_users.into_iter().collect(),
                slack_cfg.allow_bot_messages,
                slack_cfg.trusted_bot_ids.into_iter().collect(),
                slack_cfg.allow_user_messages,
                max_bot_turns,
                stt,
                slack_shutdown_rx,
                slack_dispatcher,
            )
            .await
            {
                error!("slack adapter error: {e}");
            }
        }))
    } else {
        None
    };

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
            streaming: gw_cfg.streaming,
        };
        let gw_router = router.clone();
        Some(tokio::spawn(async move {
            if let Err(e) =
                gateway::run_gateway_adapter(params, shutdown_rx, gw_dispatcher, gw_router).await
            {
                error!("gateway adapter error: {e}");
            }
        }))
    } else {
        None
    };

    // Spawn cron scheduler (background task) — reuses shared adapters
    let usercron_path = if cfg.cron.usercron_enabled {
        cfg.cron.usercron_path.as_ref().map(|p| {
            let path = std::path::PathBuf::from(p);
            if path.is_absolute() {
                path
            } else {
                // Relative paths resolve from $HOME/.openab/ (e.g. "cronjob.toml" → "$HOME/.openab/cronjob.toml")
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
        if let Some(ref a) = shared_slack_adapter {
            cron_adapters.insert("slack".into(), a.clone() as Arc<dyn adapter::ChatAdapter>);
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
        let allowed_role_ids = parse_id_set(&discord_cfg.allowed_role_ids, "discord.allowed_role_ids")?;
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

        let handler = discord::Handler {
            router,
            allow_all_channels,
            allow_all_users,
            allowed_channels,
            allowed_users,
            stt_config: cfg.stt.clone(),
            adapter: std::sync::OnceLock::new(),
            allow_bot_messages: discord_cfg.allow_bot_messages,
            trusted_bot_ids,
            allow_user_messages: discord_cfg.allow_user_messages,
            allowed_role_ids,
            participated_threads: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            multibot_threads: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            session_ttl: std::time::Duration::from_secs(ttl_secs),
            max_bot_turns: discord_cfg.max_bot_turns,
            bot_turns: tokio::sync::Mutex::new(bot_turns::BotTurnTracker::new(
                discord_cfg.max_bot_turns,
            )),
            allow_dm: discord_cfg.allow_dm,
            dispatcher: discord_dispatcher,
        };

        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::DIRECT_MESSAGES;

        let mut client = Client::builder(&discord_cfg.bot_token, intents)
            .event_handler(handler)
            .await?;

        // Graceful Discord shutdown on ctrl_c
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
        // No Discord — wait for SIGINT or SIGTERM
        info!("running without discord, press ctrl+c to stop");
        shutdown_signal().await;
        info!("shutdown signal received");
    }

    // Cleanup
    cleanup_handle.abort();
    // Signal Slack adapter to shut down gracefully
    let _ = shutdown_tx.send(true);
    if let Some(handle) = slack_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }
    if let Some(handle) = gateway_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }
    if let Some(handle) = cron_handle {
        // cron.rs drains in-flight tasks for up to 30s, so wait slightly longer
        let _ = tokio::time::timeout(std::time::Duration::from_secs(35), handle).await;
    }
    // Drain per-thread dispatchers and log buffered_lost counts before pool shutdown (ADR §6.8).
    for d in dispatchers.lock().unwrap().iter() {
        d.shutdown();
    }
    let shutdown_pool = pool;
    shutdown_pool.shutdown().await;
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
        assert!(cli.command.is_none()); // None → unwrap_or(Run { config: None })
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
}

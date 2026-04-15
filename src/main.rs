mod acp;
mod adapter;
mod config;
mod discord;
mod error_display;
mod format;
mod media;
mod reactions;
mod setup;
mod slack;
mod stt;

use adapter::AdapterRouter;
use clap::Parser;
use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};

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
        /// Config file path (default: config.toml)
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

    let cmd = Cli::parse().command.unwrap_or(Commands::Run { config: None });

    match cmd {
        Commands::Setup { output } => {
            setup::run_setup(output.map(PathBuf::from))?;
            Ok(())
        }
        Commands::Run { config } => {
            let config_path = config
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("config.toml"));

            let mut cfg = config::load_config(&config_path)?;
            info!(
                agent_cmd = %cfg.agent.command,
                pool_max = cfg.pool.max_sessions,
                discord = cfg.discord.is_some(),
                slack = cfg.slack.is_some(),
                reactions = cfg.reactions.enabled,
                "config loaded"
            );

            if cfg.discord.is_none() && cfg.slack.is_none() {
                anyhow::bail!("no adapter configured — add [discord] and/or [slack] to config.toml");
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

            let router = Arc::new(AdapterRouter::new(pool.clone(), cfg.reactions));

            // Shutdown signal for Slack adapter
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

            // Spawn cleanup task
            let cleanup_pool = pool.clone();
            let cleanup_handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    cleanup_pool.cleanup_idle(ttl_secs).await;
                }
            });

            // Spawn Slack adapter (background task)
            let slack_handle = if let Some(slack_cfg) = cfg.slack {
                info!(
                    channels = slack_cfg.allowed_channels.len(),
                    users = slack_cfg.allowed_users.len(),
                    "starting slack adapter"
                );
                let router = router.clone();
                let stt = cfg.stt.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = slack::run_slack_adapter(
                        slack_cfg.bot_token,
                        slack_cfg.app_token,
                        slack_cfg.allowed_channels.into_iter().collect(),
                        slack_cfg.allowed_users.into_iter().collect(),
                        stt,
                        router,
                        shutdown_rx,
                    )
                    .await
                    {
                        error!("slack adapter error: {e}");
                    }
                }))
            } else {
                None
            };

            // Run Discord adapter (foreground, blocking) or wait for ctrl_c
            if let Some(discord_cfg) = cfg.discord {
                let allowed_channels =
                    parse_id_set(&discord_cfg.allowed_channels, "discord.allowed_channels")?;
                let allowed_users = parse_id_set(&discord_cfg.allowed_users, "discord.allowed_users")?;
                let trusted_bot_ids = parse_id_set(&discord_cfg.trusted_bot_ids, "discord.trusted_bot_ids")?;
                info!(
                    channels = allowed_channels.len(),
                    users = allowed_users.len(),
                    trusted_bots = trusted_bot_ids.len(),
                    allow_bot_messages = ?discord_cfg.allow_bot_messages,
                    "starting discord adapter"
                );

                let handler = discord::Handler {
                    router,
                    allowed_channels,
                    allowed_users,
                    stt_config: cfg.stt.clone(),
                    adapter: std::sync::OnceLock::new(),
                    allow_bot_messages: discord_cfg.allow_bot_messages,
                    trusted_bot_ids,
                };

                let intents = GatewayIntents::GUILD_MESSAGES
                    | GatewayIntents::MESSAGE_CONTENT
                    | GatewayIntents::GUILDS;

                let mut client = Client::builder(&discord_cfg.bot_token, intents)
                    .event_handler(handler)
                    .await?;

                // Graceful Discord shutdown on ctrl_c
                let shard_manager = client.shard_manager.clone();
                tokio::spawn(async move {
                    tokio::signal::ctrl_c().await.ok();
                    info!("shutdown signal received");
                    shard_manager.shutdown_all().await;
                });

                info!("discord bot running");
                client.start().await?;
            } else {
                // No Discord — just wait for ctrl_c
                info!("running without discord, press ctrl+c to stop");
                tokio::signal::ctrl_c().await.ok();
                info!("shutdown signal received");
            }

            // Cleanup
            cleanup_handle.abort();
            // Signal Slack adapter to shut down gracefully
            let _ = shutdown_tx.send(true);
            if let Some(handle) = slack_handle {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
            }
            let shutdown_pool = pool;
            shutdown_pool.shutdown().await;
            info!("openab shut down");
            Ok(())
        }
    }
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

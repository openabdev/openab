mod acp;
mod config;
mod discord;
mod error_display;
mod format;
mod reactions;
mod setup;
mod stt;
mod usage;

use clap::Parser;
use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Parser)]
#[command(name = "openab")]
#[command(about = "Discord bot that manages ACP agent sessions", long_about = None)]
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

    let cmd = Cli::parse()
        .command
        .unwrap_or(Commands::Run { config: None });

    match cmd {
        Commands::Setup { output } => {
            setup::run_setup(output.map(PathBuf::from))?;
            return Ok(());
        }
        Commands::Run { config } => {
            let config_path = config
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("config.toml"));

            let mut cfg = config::load_config(&config_path)?;
            info!(
                agent_cmd = %cfg.agent.command,
                pool_max = cfg.pool.max_sessions,
                channels = ?cfg.discord.allowed_channels,
                users = ?cfg.discord.allowed_users,
                reactions = cfg.reactions.enabled,
                allow_bot_messages = ?cfg.discord.allow_bot_messages,
                "config loaded"
            );

            let backend =
                discord::BackendType::from_agent_config(&cfg.agent.command, &cfg.agent.args);
            let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));
            let ttl_secs = cfg.pool.session_ttl_hours * 3600;

            let allowed_channels = parse_id_set(&cfg.discord.allowed_channels, "allowed_channels")?;
            let allowed_users = parse_id_set(&cfg.discord.allowed_users, "allowed_users")?;
            let trusted_bot_ids = parse_id_set(&cfg.discord.trusted_bot_ids, "trusted_bot_ids")?;
            info!(channels = allowed_channels.len(), users = allowed_users.len(), trusted_bots = ?trusted_bot_ids, "parsed allowlists");

            // Resolve STT config before constructing handler (auto-detect mutates cfg.stt)
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

            let reaction_config = cfg.reactions;
            let emoji_presets = reaction_config.presets.clone();
            let handler = discord::Handler {
                pool: pool.clone(),
                allowed_channels,
                allowed_users,
                reactions_config: Arc::new(tokio::sync::RwLock::new(reaction_config)),
                emoji_presets,
                usage_config: cfg.usage,
                cusage_config: cfg.cusage,
                backend,
                copilot_list_cache: Arc::new(tokio::sync::RwLock::new(
                    std::collections::HashMap::new(),
                )),
                stt_config: cfg.stt.clone(),
                allow_bot_messages: cfg.discord.allow_bot_messages,
                trusted_bot_ids,
                soul_file: cfg.soul_file,
                mcp_profiles_dir: cfg.mcp_profiles_dir.clone(),
            };

            let intents = GatewayIntents::GUILD_MESSAGES
                | GatewayIntents::MESSAGE_CONTENT
                | GatewayIntents::GUILDS;

            let mut client = Client::builder(&cfg.discord.bot_token, intents)
                .event_handler(handler)
                .await?;

            // Spawn cleanup task
            let cleanup_pool = pool.clone();
            let cleanup_handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    cleanup_pool.cleanup_idle(ttl_secs).await;
                }
            });

            // Run bot until SIGINT/SIGTERM
            let shard_manager = client.shard_manager.clone();
            let shutdown_pool = pool.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                info!("shutdown signal received");
                shard_manager.shutdown_all().await;
            });

            info!("starting discord bot");
            client.start().await?;

            // Cleanup
            cleanup_handle.abort();
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

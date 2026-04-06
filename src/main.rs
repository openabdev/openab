mod acp;
mod config;
mod discord;
mod format;
mod management;
mod reactions;

use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::time::Instant;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent_broker=info".into()),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let cfg = config::load_config(&config_path)?;
    info!(
        agent_cmd = %cfg.agent.command,
        pool_max = cfg.pool.max_sessions,
        channels = ?cfg.discord.allowed_channels,
        reactions = cfg.reactions.enabled,
        "config loaded"
    );

    let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));
    let ttl_secs = cfg.pool.session_ttl_hours * 3600;

    let allowed_channels: HashSet<u64> = cfg
        .discord
        .allowed_channels
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();

    let discord_connected = Arc::new(AtomicBool::new(false));

    let handler = discord::Handler {
        pool: pool.clone(),
        allowed_channels,
        reactions_config: cfg.reactions,
        discord_connected: discord_connected.clone(),
    };

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;

    let mut client = Client::builder(&cfg.discord.bot_token, intents)
        .event_handler(handler)
        .await?;

    // Spawn management server
    let started = Instant::now();
    if cfg.management.enabled {
        let mgmt_pool = pool.clone();
        let mgmt_dc = discord_connected.clone();
        tokio::spawn(management::serve(cfg.management.bind, mgmt_pool, started, mgmt_dc));
    }

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
    info!("agent-broker shut down");
    Ok(())
}

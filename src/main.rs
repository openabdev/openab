mod acp;
mod config;
mod telegram;
mod format;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
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
        allowed_users = ?cfg.telegram.allowed_users,
        "config loaded"
    );

    let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));

    let allowed_users: HashSet<i64> = cfg.telegram.allowed_users.iter().cloned().collect();

    telegram::run(pool.clone(), cfg.telegram.bot_token, allowed_users).await;

    pool.shutdown().await;
    info!("agent-broker shut down");
    Ok(())
}

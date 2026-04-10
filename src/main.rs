mod acp;
mod config;
mod discord;
mod format;
mod reactions;

use base64::Engine;
use serenity::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "openab=info".into()),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    // Self-bootstrap: ensure Kiro credentials and config.toml exist before loading.
    // This makes openab independent of any external entrypoint shim — useful when
    // deployed to platforms (e.g. Zeabur) that may build the image without our
    // entrypoint wrapper. Idempotent: only acts when the target file is missing.
    bootstrap_kiro_credentials();
    if let Err(e) = bootstrap_config(&config_path) {
        warn!(error = %e, path = %config_path.display(), "config bootstrap from env vars failed");
    }

    let cfg = config::load_config(&config_path)?;
    info!(
        agent_cmd = %cfg.agent.command,
        pool_max = cfg.pool.max_sessions,
        channels = ?cfg.discord.allowed_channels,
        users = ?cfg.discord.allowed_users,
        reactions = cfg.reactions.enabled,
        "config loaded"
    );

    let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));
    let ttl_secs = cfg.pool.session_ttl_hours * 3600;

    let allowed_channels = parse_id_set(&cfg.discord.allowed_channels, "allowed_channels")?;
    let allowed_users = parse_id_set(&cfg.discord.allowed_users, "allowed_users")?;
    info!(channels = allowed_channels.len(), users = allowed_users.len(), "parsed allowlists");

    let handler = discord::Handler {
        pool: pool.clone(),
        allowed_channels,
        allowed_users,
        reactions_config: cfg.reactions,
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

/// Restore Kiro CLI credentials from KIRO_CRED_B64 if the target file is missing.
/// No-op when the env var is unset or the file already exists (e.g. mounted via volume).
fn bootstrap_kiro_credentials() {
    let b64 = match std::env::var("KIRO_CRED_B64") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            info!("[bootstrap] KIRO_CRED_B64 not set, skipping kiro-cli credential restore");
            return;
        }
    };
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => {
            warn!("[bootstrap] HOME not set, cannot restore kiro-cli credentials");
            return;
        }
    };
    let target_dir = home.join(".local/share/kiro-cli");
    let target_file = target_dir.join("data.sqlite3");
    if target_file.exists() && std::fs::metadata(&target_file).map(|m| m.len() > 0).unwrap_or(false) {
        info!(path = %target_file.display(), "[bootstrap] kiro-cli credentials already present, skipping restore");
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        warn!(error = %e, "[bootstrap] failed to create kiro-cli data dir");
        return;
    }
    let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "[bootstrap] KIRO_CRED_B64 is not valid base64");
            return;
        }
    };
    if let Err(e) = std::fs::write(&target_file, &bytes) {
        warn!(error = %e, "[bootstrap] failed to write kiro-cli credentials");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(0o600));
    }
    info!(path = %target_file.display(), bytes = bytes.len(), "[bootstrap] restored kiro-cli credentials from KIRO_CRED_B64");
}

/// Generate config.toml from environment variables if it doesn't exist.
/// Secrets are written as literal `${VAR}` placeholders — they are expanded by
/// `config::load_config()` at read time, so the bot token never lands on disk.
fn bootstrap_config(config_path: &Path) -> anyhow::Result<()> {
    if config_path.exists() {
        info!(path = %config_path.display(), "[bootstrap] config exists, skipping generation");
        return Ok(());
    }
    if std::env::var("DISCORD_BOT_TOKEN").is_err() {
        info!("[bootstrap] DISCORD_BOT_TOKEN not set, skipping config generation");
        return Ok(());
    }
    let channel = std::env::var("DISCORD_CHANNEL_ID").unwrap_or_default();
    let template = format!(
        r#"[discord]
bot_token = "${{DISCORD_BOT_TOKEN}}"
allowed_channels = ["{channel}"]

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

[pool]
max_sessions = 10
session_ttl_hours = 24

[reactions]
enabled = true
remove_after_reply = false

[reactions.emojis]
queued = "👀"
thinking = "🤔"
tool = "🔥"
coding = "👨‍💻"
web = "⚡"
done = "🆗"
error = "😱"

[reactions.timing]
debounce_ms = 700
stall_soft_ms = 10000
stall_hard_ms = 30000
done_hold_ms = 1500
error_hold_ms = 2500
"#
    );
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, template)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }
    info!(path = %config_path.display(), "[bootstrap] generated config.toml from environment variables");
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
        anyhow::bail!("all {label} entries failed to parse — refusing to start with an empty allowlist");
    }
    Ok(set)
}

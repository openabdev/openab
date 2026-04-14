mod acp;
mod config;
mod discord;
mod error_display;
mod format;
mod reactions;
mod stt;
mod usage;

use serenity::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
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

    let mut cfg = config::load_config(&config_path)?;
    info!(
        agent_cmd = %cfg.agent.command,
        pool_max = cfg.pool.max_sessions,
        channels = ?cfg.discord.allowed_channels,
        users = ?cfg.discord.allowed_users,
        reactions = cfg.reactions.enabled,
        "config loaded"
    );

    let backend = discord::BackendType::from_agent_config(&cfg.agent.command, &cfg.agent.args);
    info!(?backend, "detected backend type");

    let pool = Arc::new(acp::SessionPool::new(cfg.agent, cfg.pool.max_sessions));
    let ttl_secs = cfg.pool.session_ttl_hours * 3600;

    let allowed_channels = parse_id_set(&cfg.discord.allowed_channels, "allowed_channels")?;
    let allowed_users = parse_id_set(&cfg.discord.allowed_users, "allowed_users")?;
    info!(channels = allowed_channels.len(), users = allowed_users.len(), "parsed allowlists");

    let copilot_list_cache = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));

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

    let handler = discord::Handler {
        pool: pool.clone(),
        allowed_channels,
        allowed_users,
        emoji_presets: cfg.reactions.presets.clone(),
        reactions_config: Arc::new(tokio::sync::RwLock::new(cfg.reactions)),
        usage_config: cfg.usage,
        cusage_config: cfg.cusage,
        backend,
        copilot_list_cache: copilot_list_cache.clone(),
        stt_config: cfg.stt.clone(),
        soul_file: cfg.soul_file.clone(),
        mcp_profiles_dir: cfg.mcp_profiles_dir.clone(),
    };

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;

    let mut client = Client::builder(&cfg.discord.bot_token, intents)
        .event_handler(handler)
        .await?;

    // Warmup: spawn a background session so the model cache is populated
    // before the first /model autocomplete fires. Without this, the first user
    // to open the autocomplete picker would see an empty list (since spawning
    // an agent takes ~10s — far over Discord's 3s autocomplete deadline).
    let warmup_pool = pool.clone();
    tokio::spawn(async move {
        info!("[warmup] preloading model cache");
        match warmup_pool.get_or_create("__warmup__").await {
            Ok(()) => {
                let count = warmup_pool.cached_models().await.len();
                info!(count, "[warmup] model cache populated");
            }
            Err(e) => warn!(error = %e, "[warmup] failed to preload model cache"),
        }
    });

    // Spawn cleanup task
    let cleanup_pool = pool.clone();
    let cleanup_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cleanup_pool.cleanup_idle(ttl_secs).await;
        }
    });

    // Spawn Copilot list cache refresh task. Polls copilot-rpc.js for
    // agents/skills/mcp/extensions lists every 5 minutes and stores the
    // name strings in `copilot_list_cache` for instant autocomplete lookup.
    // Also refreshes the pool's cached_models snapshot used by /model autocomplete.
    let list_cache_refresh = copilot_list_cache.clone();
    let list_cache_pool = pool.clone();
    let list_cache_handle = tokio::spawn(async move {
        // Initial delay so bridge/ACP session has time to warm up
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop {
            refresh_copilot_list_cache(&list_cache_refresh).await;
            refresh_copilot_models(&list_cache_pool).await;
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
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
    list_cache_handle.abort();
    shutdown_pool.shutdown().await;
    info!("openab shut down");
    Ok(())
}

/// Background refresh for pool.cached_models used by /model autocomplete.
/// Calls copilot-rpc.js `models` and feeds the result into the pool so that
/// models added or removed upstream are reflected in `/model` suggestions
/// without needing to restart OpenAB.
async fn refresh_copilot_models(pool: &Arc<acp::SessionPool>) {
    use tokio::process::Command as TokioCommand;
    let script = r"C:\Users\Administrator\openab\scripts\copilot-rpc.js";
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        TokioCommand::new("node").arg(script).arg("models").output(),
    )
    .await;
    let Ok(Ok(output)) = out else { return };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(json_line) = stdout.lines().rev().find(|l| l.trim().starts_with('{')) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_line.trim()) else {
        return;
    };
    let Some(arr) = v.pointer("/data/models").and_then(|a| a.as_array()) else {
        return;
    };
    let models: Vec<acp::connection::ModelInfo> = arr
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|v| v.as_str())?;
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();
            Some(acp::connection::ModelInfo {
                model_id: id.to_string(),
                name,
                description: String::new(),
            })
        })
        .collect();
    tracing::info!(count = models.len(), "[refresh] pool cached_models updated");
    pool.set_cached_models(models).await;
}

/// Background refresh for the Copilot list cache. Calls copilot-rpc.js
/// for each list RPC and extracts item names. Silently ignores errors
/// (the /xxx slash commands fall back to "no matches" if cache is empty).
async fn refresh_copilot_list_cache(
    cache: &Arc<tokio::sync::RwLock<std::collections::HashMap<String, Vec<String>>>>,
) {
    use tokio::process::Command as TokioCommand;

    let script = r"C:\Users\Administrator\openab\scripts\copilot-rpc.js";
    let lists: &[(&str, &str, &str)] = &[
        // (rpc_subcommand, json_array_key, item_name_field)
        ("agents",     "agents",     "name"),
        ("skills",     "skills",     "name"),
        ("mcp-list",   "servers",    "name"),
        ("extensions", "extensions", "name"),
    ];

    for (rpc, array_key, name_key) in lists {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            TokioCommand::new("node").arg(script).arg(rpc).output(),
        )
        .await;

        let Ok(Ok(output)) = out else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(json_line) = stdout.lines().rev().find(|l| l.trim().starts_with('{')) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json_line.trim()) else {
            continue;
        };
        let Some(arr) = v.pointer(&format!("/data/{array_key}")).and_then(|a| a.as_array())
        else {
            continue;
        };
        let names: Vec<String> = arr
            .iter()
            .filter_map(|i| i.get(name_key).and_then(|n| n.as_str()).map(String::from))
            .collect();
        tracing::info!(rpc = %rpc, count = names.len(), "[refresh] list cache updated");
        cache.write().await.insert((*rpc).to_string(), names);
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
        anyhow::bail!("all {label} entries failed to parse — refusing to start with an empty allowlist");
    }
    Ok(set)
}

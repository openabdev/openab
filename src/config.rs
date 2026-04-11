use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub discord: DiscordConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub reactions: ReactionsConfig,
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    #[serde(default = "default_auto_archive_duration")]
    pub auto_archive_duration: u32,
    #[serde(default = "default_true")]
    pub require_mention: bool,
    #[serde(default)]
    pub ignore_other_mentions: bool,
    #[serde(default = "default_thread_name_mode")]
    pub thread_name_mode: String,
    #[serde(default)]
    pub channels: HashMap<String, ChannelConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ChannelConfig {
    pub auto_archive_duration: Option<u32>,
    pub require_mention: Option<bool>,
    pub ignore_other_mentions: Option<bool>,
    pub thread_name_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_working_dir")]
    pub working_dir: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct PoolConfig {
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_ttl_hours")]
    pub session_ttl_hours: u64,
}

#[derive(Debug, Deserialize)]
pub struct ReactionsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub remove_after_reply: bool,
    #[serde(default)]
    pub emojis: ReactionEmojis,
    #[serde(default)]
    pub timing: ReactionTiming,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactionEmojis {
    #[serde(default = "emoji_queued")]
    pub queued: String,
    #[serde(default = "emoji_thinking")]
    pub thinking: String,
    #[serde(default = "emoji_tool")]
    pub tool: String,
    #[serde(default = "emoji_coding")]
    pub coding: String,
    #[serde(default = "emoji_web")]
    pub web: String,
    #[serde(default = "emoji_done")]
    pub done: String,
    #[serde(default = "emoji_error")]
    pub error: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactionTiming {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_stall_soft_ms")]
    pub stall_soft_ms: u64,
    #[serde(default = "default_stall_hard_ms")]
    pub stall_hard_ms: u64,
    #[serde(default = "default_done_hold_ms")]
    pub done_hold_ms: u64,
    #[serde(default = "default_error_hold_ms")]
    pub error_hold_ms: u64,
}

// --- defaults ---

fn default_auto_archive_duration() -> u32 { 1440 }
fn default_thread_name_mode() -> String { "truncate".into() }
fn default_working_dir() -> String { "/tmp".into() }
fn default_max_sessions() -> usize { 10 }
fn default_ttl_hours() -> u64 { 24 }
fn default_true() -> bool { true }

fn emoji_queued() -> String { "👀".into() }
fn emoji_thinking() -> String { "🤔".into() }
fn emoji_tool() -> String { "🔥".into() }
fn emoji_coding() -> String { "👨‍💻".into() }
fn emoji_web() -> String { "⚡".into() }
fn emoji_done() -> String { "🆗".into() }
fn emoji_error() -> String { "😱".into() }

fn default_debounce_ms() -> u64 { 700 }
fn default_stall_soft_ms() -> u64 { 10_000 }
fn default_stall_hard_ms() -> u64 { 30_000 }
fn default_done_hold_ms() -> u64 { 1_500 }
fn default_error_hold_ms() -> u64 { 2_500 }

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_sessions: default_max_sessions(), session_ttl_hours: default_ttl_hours() }
    }
}

impl Default for ReactionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remove_after_reply: false,
            emojis: ReactionEmojis::default(),
            timing: ReactionTiming::default(),
        }
    }
}

impl Default for ReactionEmojis {
    fn default() -> Self {
        Self {
            queued: emoji_queued(), thinking: emoji_thinking(), tool: emoji_tool(),
            coding: emoji_coding(), web: emoji_web(), done: emoji_done(), error: emoji_error(),
        }
    }
}

impl Default for ReactionTiming {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(), stall_soft_ms: default_stall_soft_ms(),
            stall_hard_ms: default_stall_hard_ms(), done_hold_ms: default_done_hold_ms(),
            error_hold_ms: default_error_hold_ms(),
        }
    }
}

// --- loading ---

fn expand_env_vars(raw: &str) -> String {
    let re = Regex::new(r"\$\{(\w+)\}").unwrap();
    re.replace_all(raw, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_default()
    })
    .into_owned()
}

pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let expanded = expand_env_vars(&raw);
    let config: Config = toml::from_str(&expanded)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    const VALID_ARCHIVE_DURATIONS: &[u32] = &[60, 1440, 4320, 10080];
    if !VALID_ARCHIVE_DURATIONS.contains(&config.discord.auto_archive_duration) {
        anyhow::bail!(
            "invalid auto_archive_duration: {}. Must be one of: 60 (1h), 1440 (1d), 4320 (3d), 10080 (1w)",
            config.discord.auto_archive_duration
        );
    }
    const VALID_THREAD_NAME_MODES: &[&str] = &["truncate", "generated"];
    if !VALID_THREAD_NAME_MODES.contains(&config.discord.thread_name_mode.as_str()) {
        anyhow::bail!(
            "invalid thread_name_mode: \"{}\". Must be one of: truncate, generated",
            config.discord.thread_name_mode
        );
    }
    for (channel_id, channel_cfg) in &config.discord.channels {
        if let Some(duration) = channel_cfg.auto_archive_duration {
            if !VALID_ARCHIVE_DURATIONS.contains(&duration) {
                anyhow::bail!(
                    "invalid auto_archive_duration for channel {}: {}. Must be one of: 60 (1h), 1440 (1d), 4320 (3d), 10080 (1w)",
                    channel_id,
                    duration
                );
            }
        }
        if let Some(ref mode) = channel_cfg.thread_name_mode {
            if !VALID_THREAD_NAME_MODES.contains(&mode.as_str()) {
                anyhow::bail!(
                    "invalid thread_name_mode for channel {}: \"{}\". Must be one of: truncate, generated",
                    channel_id,
                    mode
                );
            }
        }
    }
    Ok(config)
}

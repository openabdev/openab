use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// An MCP server entry injected into ACP session/new.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(flatten)]
    pub config: serde_json::Value,
}

/// Read MCP server entries from a user's profile JSON.
/// Returns an empty vec on any error (graceful fallback).
pub fn read_mcp_profile(profiles_dir: &str, user_id: &str) -> Vec<McpServerEntry> {
    let path = std::path::PathBuf::from(profiles_dir).join(format!("{user_id}.json"));
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let servers = match parsed.get("mcpServers").and_then(|v| v.as_object()) {
        Some(map) => map,
        None => return vec![],
    };
    servers
        .iter()
        .map(|(name, config)| McpServerEntry {
            name: name.clone(),
            config: config.clone(),
        })
        .collect()
}

/// Controls whether the bot processes messages from other Discord bots.
///
/// Inspired by Hermes Agent's `DISCORD_ALLOW_BOTS` 3-value design:
/// - `Off` (default): ignore all bot messages (safe default, no behavior change)
/// - `Mentions`: only process bot messages that @mention this bot (natural loop breaker)
/// - `All`: process all bot messages (capped at `MAX_CONSECUTIVE_BOT_TURNS`)
///
/// The bot's own messages are always ignored regardless of this setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AllowBots {
    #[default]
    Off,
    Mentions,
    All,
}

impl<'de> Deserialize<'de> for AllowBots {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "off" | "none" | "false" => Ok(Self::Off),
            "mentions" => Ok(Self::Mentions),
            "all" | "true" => Ok(Self::All),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["off", "mentions", "all"],
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub discord: DiscordConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub reactions: ReactionsConfig,
    #[serde(default)]
    pub stt: SttConfig,
    /// Optional usage dashboard command config (`/usage`).
    #[serde(default)]
    pub usage: Option<UsageConfig>,
    /// Optional custom usage dashboard config (`/cusage`).
    #[serde(default)]
    pub cusage: Option<UsageConfig>,
    /// Optional path to a soul/persona text file shown via `/soul`.
    #[serde(default)]
    pub soul_file: Option<String>,
    /// Directory for per-user MCP profiles.
    #[serde(default)]
    pub mcp_profiles_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_usage_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub runners: Vec<UsageRunnerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageRunnerConfig {
    pub name: String,
    pub label: String,
    #[serde(default = "default_usage_color")]
    pub color: u32,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub template: String,
    #[serde(default)]
    pub progress_fields: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SttConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_stt_model")]
    pub model: String,
    #[serde(default = "default_stt_base_url")]
    pub base_url: String,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            model: default_stt_model(),
            base_url: default_stt_base_url(),
        }
    }
}

fn default_stt_model() -> String {
    "whisper-large-v3-turbo".into()
}
fn default_stt_base_url() -> String {
    "https://api.groq.com/openai/v1".into()
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub allow_bot_messages: AllowBots,
    /// When non-empty, only bot messages from these IDs pass the bot gate.
    /// Combines with `allow_bot_messages`: the mode check runs first, then
    /// the allowlist filters further. Empty = allow any bot (mode permitting).
    /// Only relevant when `allow_bot_messages` is `"mentions"` or `"all"`;
    /// ignored when `"off"` since all bot messages are rejected before this check.
    #[serde(default)]
    pub trusted_bot_ids: Vec<String>,
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
    #[serde(default)]
    pub presets: Vec<EmojiPreset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmojiPreset {
    pub name: String,
    pub emojis: ReactionEmojis,
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

fn default_working_dir() -> String {
    "/tmp".into()
}
fn default_max_sessions() -> usize {
    10
}
fn default_ttl_hours() -> u64 {
    4
}
fn default_true() -> bool {
    true
}
fn default_usage_timeout() -> u64 {
    30
}
fn default_usage_color() -> u32 {
    0x5865F2
}

fn emoji_queued() -> String {
    "👀".into()
}
fn emoji_thinking() -> String {
    "🤔".into()
}
fn emoji_tool() -> String {
    "🔥".into()
}
fn emoji_coding() -> String {
    "👨‍💻".into()
}
fn emoji_web() -> String {
    "⚡".into()
}
fn emoji_done() -> String {
    "🆗".into()
}
fn emoji_error() -> String {
    "😱".into()
}

fn default_debounce_ms() -> u64 {
    700
}
fn default_stall_soft_ms() -> u64 {
    10_000
}
fn default_stall_hard_ms() -> u64 {
    30_000
}
fn default_done_hold_ms() -> u64 {
    1_500
}
fn default_error_hold_ms() -> u64 {
    2_500
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_sessions: default_max_sessions(),
            session_ttl_hours: default_ttl_hours(),
        }
    }
}

impl Default for ReactionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remove_after_reply: false,
            emojis: ReactionEmojis::default(),
            timing: ReactionTiming::default(),
            presets: Vec::new(),
        }
    }
}

impl Default for ReactionEmojis {
    fn default() -> Self {
        Self {
            queued: emoji_queued(),
            thinking: emoji_thinking(),
            tool: emoji_tool(),
            coding: emoji_coding(),
            web: emoji_web(),
            done: emoji_done(),
            error: emoji_error(),
        }
    }
}

impl Default for ReactionTiming {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce_ms(),
            stall_soft_ms: default_stall_soft_ms(),
            stall_hard_ms: default_stall_hard_ms(),
            done_hold_ms: default_done_hold_ms(),
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
    Ok(config)
}

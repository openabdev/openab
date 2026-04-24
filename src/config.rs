use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

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
            other => Err(serde::de::Error::unknown_variant(other, &["off", "mentions", "all"])),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
    pub agent: AgentConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub reactions: ReactionsConfig,
    #[serde(default)]
    pub stt: SttConfig,
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

fn default_stt_model() -> String { "whisper-large-v3-turbo".into() }
fn default_stt_base_url() -> String { "https://api.groq.com/openai/v1".into() }

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    /// Explicit flag: true = allow all channels, false = check allowed_channels list.
    /// When not set, auto-detected: non-empty list → false, empty list → true.
    pub allow_all_channels: Option<bool>,
    /// Explicit flag: true = allow all users, false = check allowed_users list.
    /// When not set, auto-detected: non-empty list → false, empty list → true.
    pub allow_all_users: Option<bool>,
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
    #[serde(default)]
    pub allow_user_messages: AllowUsers,
    /// Max consecutive bot turns (without human intervention) before throttling.
    /// Human message resets the counter. Default: 20.
    #[serde(default = "default_max_bot_turns")]
    pub max_bot_turns: u32,
}

fn default_max_bot_turns() -> u32 { 20 }

/// Controls whether the bot responds to user messages in threads without @mention.
///
/// - `Involved` (default): respond to thread messages only if the bot has participated
///   in the thread (posted at least one message, or the thread parent @mentions the bot).
///   Channel/MPDM messages always require @mention. DMs always process (implicit mention).
/// - `Mentions`: always require @mention, even in threads the bot is participating in.
/// - `MultibotMentions`: same as `Involved` in single-bot threads; falls back to `Mentions`
///   when other bots have also posted in the thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AllowUsers {
    #[default]
    Involved,
    Mentions,
    MultibotMentions,
}

impl<'de> Deserialize<'de> for AllowUsers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().replace('-', "_").as_str() {
            "involved" => Ok(Self::Involved),
            "mentions" => Ok(Self::Mentions),
            "multibot_mentions" => Ok(Self::MultibotMentions),
            other => Err(serde::de::Error::unknown_variant(other, &["involved", "mentions", "multibot-mentions"])),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: String,
    /// Explicit flag: true = allow all channels, false = check allowed_channels list.
    /// When not set, auto-detected: non-empty list → false, empty list → true.
    pub allow_all_channels: Option<bool>,
    /// Explicit flag: true = allow all users, false = check allowed_users list.
    /// When not set, auto-detected: non-empty list → false, empty list → true.
    pub allow_all_users: Option<bool>,
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub allow_bot_messages: AllowBots,
    /// Bot User IDs (U...) allowed to interact when allow_bot_messages is
    /// "mentions" or "all". Find via Slack UI: click bot profile → Copy member ID.
    /// Empty = allow any bot (mode permitting).
    #[serde(default)]
    pub trusted_bot_ids: Vec<String>,
    #[serde(default)]
    pub allow_user_messages: AllowUsers,
    /// Max consecutive bot turns (without human intervention) before throttling.
    /// Human message resets the counter. Default: 20.
    #[serde(default = "default_max_bot_turns")]
    pub max_bot_turns: u32,
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

fn default_working_dir() -> String { "/tmp".into() }
fn default_max_sessions() -> usize { 10 }
fn default_ttl_hours() -> u64 { 4 }
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

/// Resolve an allow_all flag: if explicitly set, use it; otherwise infer from the list.
/// Non-empty list → false (respect the list), empty list → true (allow all).
pub fn resolve_allow_all(flag: Option<bool>, list: &[String]) -> bool {
    flag.unwrap_or(list.is_empty())
}

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
    parse_config(&raw, path.display().to_string().as_str())
}

pub async fn load_config_from_url(url: &str) -> anyhow::Result<Config> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to fetch remote config from {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("remote config request to {url} returned HTTP {status}");
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("failed to read response body from {url}: {e}"))?;
    const MAX_CONFIG_BYTES: usize = 1024 * 1024; // 1 MiB
    if bytes.len() > MAX_CONFIG_BYTES {
        anyhow::bail!(
            "remote config from {url} exceeds 1 MiB limit ({} bytes)",
            bytes.len()
        );
    }
    let raw = String::from_utf8(bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("remote config from {url} is not valid UTF-8: {e}"))?;
    parse_config(&raw, url)
}

fn parse_config(raw: &str, source: &str) -> anyhow::Result<Config> {
    let expanded = expand_env_vars(raw);
    let config: Config = toml::from_str(&expanded)
        .map_err(|e| anyhow::anyhow!("failed to parse config from {source}: {e}"))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MINIMAL_TOML: &str = r#"
[discord]
bot_token = "test-token"

[agent]
command = "echo"
"#;

    #[test]
    fn parse_minimal_config() {
        let cfg = parse_config(MINIMAL_TOML, "test").unwrap();
        assert_eq!(cfg.discord.unwrap().bot_token, "test-token");
        assert_eq!(cfg.agent.command, "echo");
        assert_eq!(cfg.pool.max_sessions, 10);
        assert!(cfg.reactions.enabled);
    }

    #[test]
    fn expand_env_vars_replaces_known_var() {
        std::env::set_var("AB_TEST_VAR", "hello");
        let result = expand_env_vars("token=${AB_TEST_VAR}");
        assert_eq!(result, "token=hello");
        std::env::remove_var("AB_TEST_VAR");
    }

    #[test]
    fn expand_env_vars_unknown_becomes_empty() {
        let result = expand_env_vars("token=${AB_NONEXISTENT_12345}");
        assert_eq!(result, "token=");
    }

    #[test]
    fn expand_env_vars_in_config() {
        std::env::set_var("AB_TEST_TOKEN", "secret-bot-token");
        let toml = r#"
[discord]
bot_token = "${AB_TEST_TOKEN}"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(cfg.discord.unwrap().bot_token, "secret-bot-token");
        std::env::remove_var("AB_TEST_TOKEN");
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = parse_config("not valid toml {{{}}", "test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to parse config from test"));
    }

    #[test]
    fn load_config_missing_file_returns_error() {
        let result = load_config(Path::new("/tmp/agent-broker-nonexistent.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to read"));
    }

    #[test]
    fn load_config_from_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", MINIMAL_TOML).unwrap();
        let cfg = load_config(tmp.path()).unwrap();
        assert_eq!(cfg.discord.unwrap().bot_token, "test-token");
    }

    #[tokio::test]
    async fn load_config_from_url_invalid_host() {
        let result = load_config_from_url("https://invalid.test.example/config.toml").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to fetch remote config"));
    }
}

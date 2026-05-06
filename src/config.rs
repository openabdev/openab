use crate::markdown::TableMode;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Controls how incoming messages are dispatched to ACP turns.
///
/// - `Message` (default): each message becomes its own ACP turn (v0.8.2-beta.1 behaviour).
/// - `Thread`: one buffer per thread; all senders in a thread share a single batch and
///   produce one ACP turn per turn boundary.
/// - `Lane`: one buffer per (thread, sender); each sender batches independently and gets
///   its own ACP turn — no silent-drop risk when multiple senders address the same thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MessageProcessingMode {
    #[default]
    Message,
    Thread,
    Lane,
}

impl<'de> Deserialize<'de> for MessageProcessingMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().replace('-', "_").as_str() {
            "per_message" => Ok(Self::Message),
            "per_thread" => Ok(Self::Thread),
            "per_lane" => Ok(Self::Lane),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["per-message", "per-thread", "per-lane"],
            )),
        }
    }
}

/// Controls whether the bot processes messages from other Discord bots.
///
/// Inspired by Hermes Agent's `DISCORD_ALLOW_BOTS` 3-value design:
/// - `Off` (default): ignore all bot messages (safe default, no behavior change)
/// - `Mentions`: only process bot messages that @mention this bot (natural loop breaker)
/// - `All`: process all bot messages (hard-capped at 1000 consecutive bot turns)
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
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
    pub gateway: Option<GatewayConfig>,
    pub agent: AgentConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub reactions: ReactionsConfig,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub markdown: MarkdownConfig,
    #[serde(default)]
    pub cron: CronConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CronConfig {
    /// Enable usercron hot-reload (default: false). Must be explicitly set to true.
    #[serde(default)]
    pub usercron_enabled: bool,
    /// Path to an external cronjob.toml for hot-reloadable user-managed schedules.
    pub usercron_path: Option<String>,
    /// Baseline cronjob definitions: `[[cron.jobs]]`
    #[serde(default)]
    pub jobs: Vec<CronJobConfig>,
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
    /// Echo the transcribed text back to the thread (no mentions) before
    /// dispatching the prompt to the agent. Lets users verify STT accuracy.
    #[serde(default = "default_echo_transcript")]
    pub echo_transcript: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            model: default_stt_model(),
            base_url: default_stt_base_url(),
            echo_transcript: default_echo_transcript(),
        }
    }
}

fn default_stt_model() -> String {
    "whisper-large-v3-turbo".into()
}
fn default_stt_base_url() -> String {
    "https://api.groq.com/openai/v1".into()
}
fn default_echo_transcript() -> bool {
    false
}

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
    /// Human message resets the counter. Default: 100.
    #[serde(default = "default_max_bot_turns")]
    pub max_bot_turns: u32,
    /// Role IDs that trigger the bot (same as direct @mention).
    /// When a message mentions a role in this list, it is treated as a bot trigger.
    /// Empty (default) = role mentions do not trigger the bot.
    #[serde(default)]
    pub allowed_role_ids: Vec<String>,
    /// Allow the bot to respond to Discord direct messages (DMs).
    /// Default: false (opt-in). `allowed_users` still applies in DMs.
    #[serde(default)]
    pub allow_dm: bool,
    /// Message dispatch mode. Default: per-message (v0.8.2-beta.1 behaviour).
    #[serde(default)]
    pub message_processing_mode: MessageProcessingMode,
    /// Batched mode only: per-thread channel capacity. Default: 10.
    #[serde(default = "default_max_buffered_messages")]
    pub max_buffered_messages: usize,
    /// Batched mode only: soft token cap for greedy drain. Default: 24000.
    #[serde(default = "default_max_batch_tokens")]
    pub max_batch_tokens: usize,
}

fn default_max_bot_turns() -> u32 {
    100
}
fn default_max_buffered_messages() -> usize {
    10
}
fn default_max_batch_tokens() -> usize {
    24_000
}

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
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["involved", "mentions", "multibot-mentions"],
            )),
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
    /// Human message resets the counter. Default: 100.
    #[serde(default = "default_max_bot_turns")]
    pub max_bot_turns: u32,
    /// Message dispatch mode. Default: per-message.
    #[serde(default)]
    pub message_processing_mode: MessageProcessingMode,
    /// Batched mode only: per-thread channel capacity. Default: 10.
    #[serde(default = "default_max_buffered_messages")]
    pub max_buffered_messages: usize,
    /// Batched mode only: soft token cap for greedy drain. Default: 24000.
    #[serde(default = "default_max_batch_tokens")]
    pub max_batch_tokens: usize,
}

#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    /// WebSocket URL of the custom gateway (e.g. ws://gateway:8080/ws)
    pub url: String,
    /// Platform name for session key namespacing (e.g. "telegram", "line")
    #[serde(default = "default_gateway_platform")]
    pub platform: String,
    /// Shared token for WebSocket authentication (optional but recommended)
    pub token: Option<String>,
    /// Bot username for @mention gating in groups (e.g. "my_bot")
    pub bot_username: Option<String>,
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
    /// Enable streaming (typewriter) mode — requires gateway platform to support message editing.
    #[serde(default)]
    pub streaming: bool,
    /// Message dispatch mode. Default: per-message.
    #[serde(default)]
    pub message_processing_mode: MessageProcessingMode,
    /// Batched mode only: per-thread channel capacity. Default: 10.
    #[serde(default = "default_max_buffered_messages")]
    pub max_buffered_messages: usize,
    /// Batched mode only: soft token cap for greedy drain. Default: 24000.
    #[serde(default = "default_max_batch_tokens")]
    pub max_batch_tokens: usize,
}

fn default_gateway_platform() -> String {
    "telegram".into()
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
    #[serde(default)]
    pub inherit_env: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PoolConfig {
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_ttl_hours")]
    pub session_ttl_hours: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CronJobConfig {
    /// Whether this cronjob is active (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Cron expression (5-field POSIX format)
    pub schedule: String,
    /// Target channel ID
    pub channel: String,
    /// Message to send to the agent
    pub message: String,
    /// Target platform (default: "discord")
    #[serde(default = "default_cron_platform")]
    pub platform: String,
    /// Sender name for attribution (default: "openab-cron")
    #[serde(default = "default_cron_sender")]
    pub sender_name: String,
    /// Optional thread ID (post to existing thread)
    pub thread_id: Option<String>,
    /// Timezone (default: "UTC")
    #[serde(default = "default_cron_timezone")]
    pub timezone: String,
}

fn default_cron_platform() -> String {
    "discord".into()
}
fn default_cron_sender() -> String {
    "openab-cron".into()
}
fn default_cron_timezone() -> String {
    "UTC".into()
}

/// Controls how tool calls are rendered in chat messages.
///
/// - `full`: show complete tool title including arguments (default, original behavior)
/// - `compact`: show only a count summary, e.g. `✅ 3 · 🔧 1 tool(s)`
/// - `none`: hide tool lines entirely, only show final response
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolDisplay {
    #[default]
    Full,
    Compact,
    None,
}

impl<'de> Deserialize<'de> for ToolDisplay {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "compact" => Ok(Self::Compact),
            "none" | "off" | "hidden" => Ok(Self::None),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["full", "compact", "none"],
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactionsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub remove_after_reply: bool,
    #[serde(default)]
    pub tool_display: ToolDisplay,
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
            tool_display: ToolDisplay::default(),
            emojis: ReactionEmojis::default(),
            timing: ReactionTiming::default(),
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

// --- markdown ---

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MarkdownConfig {
    #[serde(default)]
    pub tables: TableMode,
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

    // Validate max_buffered_messages > 0 (tokio::sync::mpsc::channel panics on 0)
    // and max_batch_tokens > 0 (otherwise the consumer's token-cap check forces every
    // batch to size 1 — functionally per-message via a confusing path).
    if let Some(ref d) = config.discord {
        anyhow::ensure!(
            d.max_buffered_messages > 0,
            "discord.max_buffered_messages must be > 0"
        );
        anyhow::ensure!(
            d.max_batch_tokens > 0,
            "discord.max_batch_tokens must be > 0"
        );
    }
    if let Some(ref s) = config.slack {
        anyhow::ensure!(
            s.max_buffered_messages > 0,
            "slack.max_buffered_messages must be > 0"
        );
        anyhow::ensure!(s.max_batch_tokens > 0, "slack.max_batch_tokens must be > 0");
    }
    if let Some(ref g) = config.gateway {
        anyhow::ensure!(
            g.max_buffered_messages > 0,
            "gateway.max_buffered_messages must be > 0"
        );
        anyhow::ensure!(
            g.max_batch_tokens > 0,
            "gateway.max_batch_tokens must be > 0"
        );
    }

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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to parse config from test"));
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to fetch remote config"));
    }

    #[test]
    fn parse_gateway_config_defaults() {
        let toml = r#"
[gateway]
url = "ws://gw:8080/ws"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        let gw = cfg.gateway.unwrap();
        assert_eq!(gw.url, "ws://gw:8080/ws");
        assert_eq!(gw.platform, "telegram");
        assert!(gw.allowed_users.is_empty());
        assert!(gw.allowed_channels.is_empty());
        assert!(gw.allow_all_users.is_none());
        assert!(gw.allow_all_channels.is_none());
        // resolve_allow_all: empty lists → allow all
        assert!(resolve_allow_all(gw.allow_all_users, &gw.allowed_users));
        assert!(resolve_allow_all(
            gw.allow_all_channels,
            &gw.allowed_channels
        ));
    }

    #[test]
    fn parse_gateway_config_with_allowlists() {
        let toml = r#"
[gateway]
url = "ws://gw:8080/ws"
platform = "line"
allowed_users = ["U1", "U2"]
allowed_channels = ["C1"]

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        let gw = cfg.gateway.unwrap();
        assert_eq!(gw.platform, "line");
        assert_eq!(gw.allowed_users, vec!["U1", "U2"]);
        assert_eq!(gw.allowed_channels, vec!["C1"]);
        // resolve_allow_all: non-empty lists → restricted
        assert!(!resolve_allow_all(gw.allow_all_users, &gw.allowed_users));
        assert!(!resolve_allow_all(
            gw.allow_all_channels,
            &gw.allowed_channels
        ));
    }

    #[test]
    fn tool_display_default_is_full() {
        assert_eq!(ToolDisplay::default(), ToolDisplay::Full);
    }

    #[test]
    fn message_processing_mode_parses_per_message() {
        let toml = r#"
[discord]
bot_token = "t"
message_processing_mode = "per-message"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(
            cfg.discord.unwrap().message_processing_mode,
            MessageProcessingMode::Message
        );
    }

    #[test]
    fn message_processing_mode_parses_per_thread() {
        let toml = r#"
[discord]
bot_token = "t"
message_processing_mode = "per-thread"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(
            cfg.discord.unwrap().message_processing_mode,
            MessageProcessingMode::Thread
        );
    }

    #[test]
    fn message_processing_mode_parses_per_lane() {
        let toml = r#"
[discord]
bot_token = "t"
message_processing_mode = "per-lane"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(
            cfg.discord.unwrap().message_processing_mode,
            MessageProcessingMode::Lane
        );
    }

    // The legacy alias "batched" was removed: only per-message / per-thread / per-lane
    // are accepted. Configs still using "batched" must migrate to an explicit value.
    #[test]
    fn message_processing_mode_batched_is_rejected() {
        let toml = r#"
[discord]
bot_token = "t"
message_processing_mode = "batched"

[agent]
command = "echo"
"#;
        assert!(parse_config(toml, "test").is_err());
    }

    #[test]
    fn message_processing_mode_default_is_per_message() {
        let cfg = parse_config(MINIMAL_TOML, "test").unwrap();
        assert_eq!(
            cfg.discord.unwrap().message_processing_mode,
            MessageProcessingMode::Message
        );
    }

    #[test]
    fn message_processing_mode_unknown_value_errors() {
        let toml = r#"
[discord]
bot_token = "t"
message_processing_mode = "bogus"

[agent]
command = "echo"
"#;
        assert!(parse_config(toml, "test").is_err());
    }

    #[test]
    fn parse_gateway_config_explicit_allow_all_overrides_list() {
        let toml = r#"
[gateway]
url = "ws://gw:8080/ws"
allow_all_users = true
allowed_users = ["U1"]

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        let gw = cfg.gateway.unwrap();
        // explicit flag overrides non-empty list
        assert!(resolve_allow_all(gw.allow_all_users, &gw.allowed_users));
    }

    #[test]
    fn stt_echo_transcript_defaults_to_false() {
        let cfg = SttConfig::default();
        assert!(
            !cfg.echo_transcript,
            "echo_transcript should default to false"
        );
    }

    #[test]
    fn stt_echo_transcript_respects_explicit_false() {
        let toml = r#"
[agent]
command = "echo"

[stt]
enabled = true
api_key = "test"
echo_transcript = false
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert!(cfg.stt.enabled);
        assert!(!cfg.stt.echo_transcript);
    }
}

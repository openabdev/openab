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

#[derive(Debug, Clone, Deserialize)]
pub struct AgentCoreConfig {
    /// AgentCore Runtime ARN (required)
    pub runtime_arn: String,
    /// ACP agent command to run in the PTY shell (default: kiro-cli acp --trust-all-tools)
    #[serde(default = "default_agentcore_shell_command")]
    pub shell_command: String,
    /// Cancel strategy: "noop" or "stop" (default: stop)
    #[serde(default = "default_agentcore_cancel_strategy")]
    #[allow(dead_code)]
    pub cancel_strategy: AgentCoreCancelStrategy,
}

fn default_agentcore_shell_command() -> String {
    "kiro-cli acp --trust-all-tools".to_string()
}

impl AgentCoreConfig {
    /// Extract region from ARN: arn:aws:bedrock-agentcore:REGION:ACCOUNT:runtime/ID
    pub fn region(&self) -> String {
        let parts: Vec<&str> = self.runtime_arn.split(':').collect();
        if parts.len() >= 4 && !parts[3].is_empty() {
            return parts[3].to_string();
        }
        "us-east-1".into() // fallback (should never hit with valid ARN)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentCoreCancelStrategy {
    #[default]
    Stop,
    Noop,
}

impl<'de> Deserialize<'de> for AgentCoreCancelStrategy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "stop" => Ok(Self::Stop),
            "noop" => Ok(Self::Noop),
            other => Err(serde::de::Error::unknown_variant(other, &["stop", "noop"])),
        }
    }
}

impl std::fmt::Display for AgentCoreCancelStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stop => write!(f, "stop"),
            Self::Noop => write!(f, "noop"),
        }
    }
}

fn default_agentcore_cancel_strategy() -> AgentCoreCancelStrategy {
    AgentCoreCancelStrategy::Stop
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
    pub gateway: Option<GatewayConfig>,
    pub telegram: Option<TelegramConfig>,
    pub agentcore: Option<AgentCoreConfig>,
    #[serde(default)]
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
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub ambient: AmbientConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceConfig {
    /// Workspace aliases: `name = "~/path/to/project"`
    /// Used with `[[ws:@alias]]` control directives.
    #[serde(default)]
    pub aliases: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretsConfig {
    /// AWS Secrets Manager configuration.
    #[serde(default)]
    pub aws: AwsSecretsConfig,
    /// Exec provider configuration.
    #[serde(default)]
    pub exec: ExecSecretsConfig,
    /// Secret references: key = "aws-sm://..." or "exec://..."
    #[serde(default)]
    pub refs: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AwsSecretsConfig {
    /// Override AWS region (otherwise uses default credential chain).
    pub region: Option<String>,
    /// Override endpoint URL (for LocalStack or VPC endpoints).
    pub endpoint_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecSecretsConfig {
    /// Per-invocation timeout in seconds (default: 10).
    #[serde(default = "default_exec_timeout")]
    pub timeout_seconds: u64,
}

impl Default for ExecSecretsConfig {
    fn default() -> Self {
        Self { timeout_seconds: 10 }
    }
}

fn default_exec_timeout() -> u64 {
    10
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HooksConfig {
    pub pre_seed: Option<PreSeedConfig>,
    pub pre_boot: Option<HookConfig>,
    pub pre_shutdown: Option<HookConfig>,
}

impl HooksConfig {
    /// Returns true if any lifecycle hook (pre_seed, pre_boot, pre_shutdown) is configured.
    pub fn any_configured(&self) -> bool {
        self.pre_seed.as_ref().is_some_and(|p| !p.sources.is_empty())
            || self.pre_boot.is_some()
            || self.pre_shutdown.is_some()
    }

    /// Fail fast if hooks are configured on an unsupported platform.
    ///
    /// Lifecycle hooks (pre_seed tarball extraction, pre_boot/pre_shutdown shell
    /// scripts) assume a Unix environment and only ever run inside Linux containers.
    /// Rather than silently misbehaving on Windows, refuse to start.
    pub fn ensure_platform_supported(&self) -> anyhow::Result<()> {
        #[cfg(not(unix))]
        {
            if self.any_configured() {
                anyhow::bail!(
                    "lifecycle hooks ([hooks.pre_seed], [hooks.pre_boot], [hooks.pre_shutdown]) \
                     are only supported on Unix platforms; remove them to run on this platform"
                );
            }
        }
        Ok(())
    }
}

/// Configuration for the pre_seed phase.
/// Downloads and extracts zip archives from S3 before pre_boot.
#[derive(Debug, Clone, Deserialize)]
pub struct PreSeedConfig {
    /// S3 URIs of zip archives to download and extract (max 5).
    /// Extracted in order; later layers overwrite earlier ones.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Extraction target directory. Default: $HOME.
    pub target: Option<String>,
    /// Override AWS region for S3 access.
    pub region: Option<String>,
    /// Override S3 endpoint URL (for LocalStack, VPC endpoints).
    pub endpoint_url: Option<String>,
    /// Maximum compressed zip size in bytes. Default: 100 MiB.
    #[serde(default = "default_max_zip_bytes")]
    pub max_bytes: u64,
    /// Timeout in seconds for each download+extract operation. Default: 300.
    #[serde(default = "default_pre_seed_timeout")]
    pub timeout_seconds: u64,
    /// Failure policy. Default: abort.
    #[serde(default)]
    pub on_failure: OnFailure,
}

impl Default for PreSeedConfig {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            target: None,
            region: None,
            endpoint_url: None,
            max_bytes: default_max_zip_bytes(),
            timeout_seconds: default_pre_seed_timeout(),
            on_failure: OnFailure::Abort,
        }
    }
}

fn default_max_zip_bytes() -> u64 {
    100 * 1024 * 1024 // 100 MiB
}

fn default_pre_seed_timeout() -> u64 {
    300
}

/// Failure policy for a hook.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnFailure {
    #[default]
    Abort,
    Warn,
}

impl<'de> Deserialize<'de> for OnFailure {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "abort" => Ok(Self::Abort),
            "warn" => Ok(Self::Warn),
            other => Err(serde::de::Error::unknown_variant(other, &["abort", "warn"])),
        }
    }
}

/// Configuration for a single hook. Exactly one of `script`, `inline`, or `url` must be set.
#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    /// Absolute path to an executable script.
    pub script: Option<String>,
    /// Inline script content (written to temp file and executed).
    pub inline: Option<String>,
    /// Remote script URL (fetched and executed).
    pub url: Option<String>,
    /// SHA-256 checksum of the remote script (required with `url`).
    pub sha256: Option<String>,
    /// Max wall-clock seconds. Default: 60.
    #[serde(default = "default_hook_timeout")]
    pub timeout_seconds: u64,
    /// Failure policy. Default: abort.
    #[serde(default)]
    pub on_failure: OnFailure,
}

fn default_hook_timeout() -> u64 {
    60
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
    ///
    /// **Admission override**: a trusted bot that explicitly @mentions this bot
    /// bypasses the `allow_bot_messages` mode entirely (treated as human @mention).
    /// This allows trusted bots to pull this bot into threads regardless of mode.
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
/// - `Involved`: respond to thread messages only if the bot has participated
///   in the thread (posted at least one message, or the thread parent @mentions the bot).
///   Channel/MPDM messages always require @mention. DMs always process (implicit mention).
/// - `Mentions`: always require @mention, even in threads the bot is participating in.
/// - `MultibotMentions` (default): same as `Involved` in single-bot threads; falls back to
///   `Mentions` when other bots have also posted in the thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AllowUsers {
    Involved,
    Mentions,
    #[default]
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
    /// Slack "AI app / Assistant" mode: stream replies via chat.startStream +
    /// assistant.threads.setStatus instead of post+edit + emoji reactions.
    /// Requires the Slack app to be an AI app (assistant feature enabled) with
    /// the `assistant:write` scope. Default: true — set to false for Slack apps
    /// that are not AI apps (no `assistant:write`) to keep emoji-reaction status.
    #[serde(default = "default_true")]
    pub assistant_mode: bool,
    /// Master streaming switch. When `false`, the Slack adapter always posts a
    /// single final message (send-once) — no native streaming, no post+edit
    /// placeholder — regardless of `assistant_mode`. Default `true`. Useful for
    /// multi-agent threads to avoid streamed-message edit states re-firing
    /// `app_mention`. Mirrors `[gateway] streaming` in concept, but the default
    /// deliberately differs: `GatewayConfig.streaming` defaults to `false`,
    /// whereas this defaults to `true` to preserve current Slack streaming.
    #[serde(default = "default_true")]
    pub streaming: bool,
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
    /// Allow messages from bots. Default: false.
    /// NOTE: Intentionally `bool` (not `AllowBots` enum) — the gateway adapter
    /// only needs on/off since @mention gating is handled separately by
    /// `bot_username` + `should_skip_event`. Discord/Slack use `AllowBots` because
    /// their adapters embed mention-mode logic internally.
    #[serde(default)]
    pub allow_bot_messages: bool,
    /// Bot IDs that bypass the bot filter even when allow_bot_messages is false.
    #[serde(default)]
    pub trusted_bot_ids: Vec<String>,
    /// Enable streaming (typewriter) mode — requires gateway platform to support message editing.
    /// Defaults to `false`, so gateway platforms (Telegram / LINE / Google Chat) are **send-once
    /// by default**. By default send-once delivers **only the final answer block** — the text after
    /// the last tool call — dropping inter-tool narration (the shared default send-once trimming in
    /// `AdapterRouter::stream_prompt_blocks`, controlled by the platform-agnostic
    /// `[reactions] narration_display`). Discord is likewise send-once in multi-bot threads
    /// (`use_streaming` = `!other_bot_present`) and gets the same default trimming. Set `true` to
    /// stream live and keep the full inter-tool text.
    #[serde(default)]
    pub streaming: bool,
    /// Show "…" placeholder at streaming start. Default: true. Set false for platforms using drafts.
    #[serde(default = "default_true")]
    pub streaming_placeholder: bool,
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

/// First-class `[telegram]` configuration section (see ADR: first-class
/// per-platform config). Config-authoritative with `${ENV}` expansion; every
/// field falls back to its `TELEGRAM_*` environment variable when unset, then to
/// a built-in default. This keeps env-only deployments working unchanged while
/// letting `config.toml` be the single source of truth.
///
/// Resolution per field: `[telegram].field` (with `${}` expansion) → `TELEGRAM_*`
/// env var → default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    /// Bot token. Env fallback: `TELEGRAM_BOT_TOKEN`.
    pub bot_token: Option<String>,
    /// Webhook secret token (L1 auth). Env fallback: `TELEGRAM_SECRET_TOKEN`.
    pub secret_token: Option<String>,
    /// Reject webhook requests whose source IP is outside Telegram's published
    /// subnets (L1). Env fallback: `TELEGRAM_TRUSTED_SOURCE_ONLY` (default false).
    pub trusted_source_only: Option<bool>,
    /// Render rich-message drafts. Env fallback: `TELEGRAM_RICH_MESSAGES`
    /// (default true).
    pub rich_messages: Option<bool>,
    /// Streaming override. When unset, streaming follows `rich_messages`.
    /// Env fallback: `TELEGRAM_STREAMING`.
    pub streaming: Option<bool>,
    /// Webhook mount path. Env fallback: `TELEGRAM_WEBHOOK_PATH`
    /// (default `/webhook/telegram`).
    pub webhook_path: Option<String>,
}

/// Fully resolved Telegram settings (config → env → default applied).
/// Plain types so the binary crate can hand them to the gateway crate without a
/// type dependency.
#[derive(Debug, Clone)]
pub struct ResolvedTelegram {
    pub bot_token: Option<String>,
    pub secret_token: Option<String>,
    pub trusted_source_only: bool,
    pub rich_messages: bool,
    pub streaming: Option<bool>,
    pub webhook_path: String,
}

impl TelegramConfig {
    /// Resolve every field: config value (if set) → `TELEGRAM_*` env → default.
    ///
    /// String fields filter out empty strings produced by `${}` expansion of
    /// unset env vars, so `bot_token = "${UNSET_VAR}"` correctly falls through
    /// to the `TELEGRAM_BOT_TOKEN` env fallback rather than holding `Some("")`.
    pub fn resolve(&self) -> ResolvedTelegram {
        ResolvedTelegram {
            bot_token: self
                .bot_token
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok()),
            secret_token: self
                .secret_token
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| std::env::var("TELEGRAM_SECRET_TOKEN").ok()),
            trusted_source_only: self
                .trusted_source_only
                .unwrap_or_else(|| env_flag_true_one("TELEGRAM_TRUSTED_SOURCE_ONLY")),
            rich_messages: self
                .rich_messages
                .unwrap_or_else(|| env_flag_not_false("TELEGRAM_RICH_MESSAGES")),
            streaming: self.streaming.or_else(|| {
                std::env::var("TELEGRAM_STREAMING")
                    .ok()
                    .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
            }),
            webhook_path: self
                .webhook_path
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| std::env::var("TELEGRAM_WEBHOOK_PATH").ok())
                .unwrap_or_else(|| "/webhook/telegram".into()),
        }
    }
}

/// `true` when env var == "1" or "true" (case-insensitive); default `false`.
/// Matches the legacy `TELEGRAM_TRUSTED_SOURCE_ONLY` semantics.
fn env_flag_true_one(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// `true` unless env var == "0" or "false" (case-insensitive); default `true`.
/// Matches the legacy `TELEGRAM_RICH_MESSAGES` semantics.
fn env_flag_not_false(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// Raw intermediate struct for serde — uses `Option` to detect explicit fields.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct AgentConfigRaw {
    command: Option<String>,
    args: Option<Vec<String>>,
    working_dir: String,
    env: HashMap<String, String>,
    inherit_env: Vec<String>,
}

impl Default for AgentConfigRaw {
    fn default() -> Self {
        Self {
            command: None,
            args: None,
            working_dir: default_working_dir(),
            env: HashMap::new(),
            inherit_env: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: String,
    pub env: HashMap<String, String>,
    pub inherit_env: Vec<String>,
    /// Whether the command was explicitly set in config (vs defaulted from env/fallback).
    pub command_explicit: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: default_agent_command(),
            args: default_agent_args(),
            working_dir: default_working_dir(),
            env: HashMap::new(),
            inherit_env: Vec::new(),
            command_explicit: false,
        }
    }
}

impl<'de> serde::Deserialize<'de> for AgentConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = AgentConfigRaw::deserialize(deserializer)?;
        let cmd_explicit = raw.command.is_some();
        let command = raw.command.unwrap_or_else(default_agent_command);
        // If command was explicitly set but args was not, default args to []
        // to avoid leaking env-var args into a custom command.
        let args = match (cmd_explicit, raw.args) {
            (_, Some(args)) => args,           // args explicitly set → use them
            (true, None) => Vec::new(),        // command set, args omitted → empty
            (false, None) => default_agent_args(), // neither set → env var
        };
        Ok(AgentConfig {
            command,
            args,
            working_dir: raw.working_dir,
            env: raw.env,
            inherit_env: raw.inherit_env,
            command_explicit: cmd_explicit,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct PoolConfig {
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_ttl_hours")]
    pub session_ttl_hours: u64,
    /// Hard ceiling for a single prompt (#732). Once exceeded, the broker
    /// abandons the in-flight request, sends `session/cancel` to the agent,
    /// and clears the pending entry so late responses cannot leak into the
    /// next prompt's subscriber.
    ///
    /// Precision: checked every `liveness_check_secs`, so actual cutoff is
    /// ±`liveness_check_secs` from this value.
    #[serde(default = "default_prompt_hard_timeout_secs")]
    pub prompt_hard_timeout_secs: u64,
    /// Polling cadence (seconds) for the recv-loop liveness check (#732).
    /// Lower = faster reaction to a dead agent / hard ceiling at the cost of
    /// more wakeups while the agent is streaming normally.
    #[serde(default = "default_liveness_check_secs")]
    pub liveness_check_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CronJobConfig {
    /// Stable ID for usercron jobs that need scheduler writeback.
    pub id: Option<String>,
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
    /// Usercron-only: command to run before firing. Exit 0 plus a matching
    /// `disable_on_success_match` means the goal is complete and the scheduler
    /// disables the job in the usercron file.
    pub disable_on_success: Option<String>,
    /// Usercron-only: required output marker for `disable_on_success`.
    pub disable_on_success_match: Option<String>,
    /// Usercron-only: timeout for `disable_on_success`.
    #[serde(default = "default_disable_on_success_timeout_secs")]
    pub disable_on_success_timeout_secs: u64,
    /// Usercron-only: working directory for `disable_on_success`.
    pub disable_on_success_working_dir: Option<String>,
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
fn default_disable_on_success_timeout_secs() -> u64 {
    60
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
    /// Whether to include the agent's inter-tool narration ("let me pull the
    /// diff", "now reading X") in send-once replies. Default `false` — a
    /// send-once turn delivers **only the final answer block** (the text after
    /// the last tool call), so the message reads like the single composed
    /// artefact a tool-posted comment is. Set `true` to keep the full text.
    ///
    /// Platform-agnostic (sits beside `tool_display`): the trimming lives in the
    /// shared adapter layer and applies to every send-once turn — Slack
    /// `streaming=false`, Slack/Discord multi-bot threads, and gateway. Only
    /// affects send-once; live streaming always shows the text as produced.
    /// Orthogonal to `streaming`, which is the per-platform stream-vs-send-once
    /// switch.
    #[serde(default)]
    pub narration_display: bool,
    #[serde(default)]
    pub emojis: ReactionEmojis,
    #[serde(default)]
    pub timing: ReactionTiming,
    /// Emoji-to-text mapping. When a user reacts with a mapped emoji,
    /// it is treated as if they sent the corresponding text message.
    #[serde(default)]
    pub mapping: HashMap<String, String>,
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
    std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
}
fn default_agent_command() -> String {
    if let Ok(val) = std::env::var("OPENAB_AGENT_COMMAND") {
        if let Some(cmd) = val.split_whitespace().next() {
            return cmd.to_string();
        }
    }
    "openab-agent".into()
}
fn default_agent_args() -> Vec<String> {
    if let Ok(val) = std::env::var("OPENAB_AGENT_COMMAND") {
        let parts: Vec<&str> = val.split_whitespace().collect();
        if parts.len() > 1 {
            return parts[1..].iter().map(|s| s.to_string()).collect();
        }
    }
    Vec::new()
}
fn default_max_sessions() -> usize {
    10
}
fn default_ttl_hours() -> u64 {
    4
}
pub(crate) fn default_prompt_hard_timeout_secs() -> u64 {
    30 * 60
}
pub(crate) fn default_liveness_check_secs() -> u64 {
    30
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
            prompt_hard_timeout_secs: default_prompt_hard_timeout_secs(),
            liveness_check_secs: default_liveness_check_secs(),
        }
    }
}

impl Default for ReactionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remove_after_reply: false,
            tool_display: ToolDisplay::default(),
            narration_display: false,
            emojis: ReactionEmojis::default(),
            timing: ReactionTiming::default(),
            mapping: HashMap::new(),
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

/// Maximum accepted size for a remotely-fetched config document (URL or S3).
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Finalize raw config bytes fetched from a remote source: enforce the size
/// cap, validate UTF-8, and expand `${ENV}` references. Shared by the URL and
/// S3 loaders so both behave identically (and so this logic is unit-testable
/// offline, without a network or the AWS SDK).
fn finalize_config_bytes(bytes: &[u8], source: &str) -> anyhow::Result<String> {
    if bytes.len() > MAX_CONFIG_BYTES {
        anyhow::bail!(
            "config from {source} exceeds 1 MiB limit ({} bytes)",
            bytes.len()
        );
    }
    let raw = std::str::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("config from {source} is not valid UTF-8: {e}"))?;
    Ok(expand_env_vars(raw))
}

/// Load raw config text from a file path (env vars expanded but secrets NOT resolved).
pub fn load_config_raw(path: &Path) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    Ok(expand_env_vars(&raw))
}

/// Load raw config text from a URL (env vars expanded but secrets NOT resolved).
pub async fn load_config_raw_from_url(url: &str) -> anyhow::Result<String> {
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
    finalize_config_bytes(&bytes, url)
}

/// Parse an `s3://<bucket>/<key>` URI into its bucket and key components.
///
/// Kept un-gated (independent of the `config-s3` feature) so URI parsing can be
/// unit-tested without pulling in the AWS SDK.
pub fn parse_s3_uri(uri: &str) -> anyhow::Result<(String, String)> {
    let rest = uri
        .strip_prefix("s3://")
        .ok_or_else(|| anyhow::anyhow!("invalid s3:// URI '{uri}' — must start with s3://"))?;
    let (bucket, key) = rest
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid s3:// URI '{uri}' — expected s3://<bucket>/<key>"))?;
    if bucket.is_empty() || key.is_empty() {
        anyhow::bail!("invalid s3:// URI '{uri}' — bucket and key must both be non-empty");
    }
    Ok((bucket.to_string(), key.to_string()))
}

/// Load raw config text from an `s3://<bucket>/<key>` URI
/// (env vars expanded but secrets NOT resolved).
///
/// Credentials/region are resolved via the standard AWS provider chain
/// (env vars, shared config, IRSA / Pod Identity / instance role), mirroring
/// how `aws-sm://` secret references are resolved.
#[cfg(feature = "config-s3")]
pub async fn load_config_raw_from_s3(uri: &str) -> anyhow::Result<String> {
    let (bucket, key) = parse_s3_uri(uri)?;
    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let client = aws_sdk_s3::Client::new(&sdk_config);
    let resp = client
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to fetch S3 config from {uri}: {e}"))?;
    let data = resp
        .body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("failed to read S3 object body from {uri}: {e}"))?
        .into_bytes();
    finalize_config_bytes(&data, uri)
}

/// Fallback when built without the `config-s3` feature: report a clear error
/// instead of failing to compile callers that dispatch on the `s3://` scheme.
#[cfg(not(feature = "config-s3"))]
pub async fn load_config_raw_from_s3(uri: &str) -> anyhow::Result<String> {
    anyhow::bail!(
        "config source '{uri}' uses the s3:// scheme, but openab was built without the 'config-s3' feature"
    )
}

/// Load raw config text from any supported source, dispatching on the scheme:
/// a local file path, an `http(s)://` URL, or an `s3://<bucket>/<key>` URI.
/// Env vars are expanded (`${VAR}`) but secrets are NOT resolved.
///
/// Centralizing scheme dispatch here keeps the binary entrypoint decoupled from
/// the set of supported config sources.
pub async fn load_config_raw_from_source(source: &str) -> anyhow::Result<String> {
    if source.starts_with("https://") {
        tracing::info!(url = %source, "fetching remote config");
        load_config_raw_from_url(source).await
    } else if source.starts_with("http://") {
        tracing::warn!(url = %source, "fetching remote config over plaintext HTTP — use HTTPS in production");
        load_config_raw_from_url(source).await
    } else if source.starts_with("s3://") {
        tracing::info!(uri = %source, "fetching config from S3");
        load_config_raw_from_s3(source).await
    } else {
        load_config_raw(Path::new(source))
    }
}

/// Parse config from already-expanded text.
pub fn parse_config_str(expanded: &str, source: &str) -> anyhow::Result<Config> {
    parse_config_inner(expanded, source)
}

#[cfg(test)]
fn parse_config(raw: &str, source: &str) -> anyhow::Result<Config> {
    let expanded = expand_env_vars(raw);
    parse_config_inner(&expanded, source)
}

#[cfg(test)]
fn load_config(path: &Path) -> anyhow::Result<Config> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    parse_config(&raw, path.display().to_string().as_str())
}

#[cfg(test)]
async fn load_config_from_url(url: &str) -> anyhow::Result<Config> {
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
    let raw = String::from_utf8(bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("remote config from {url} is not valid UTF-8: {e}"))?;
    parse_config(&raw, url)
}

fn parse_config_inner(expanded: &str, source: &str) -> anyhow::Result<Config> {
    let mut config: Config = toml::from_str(expanded)
        .map_err(|e| anyhow::anyhow!("failed to parse config from {source}: {e}"))?;

    // Resolve Discord shortcodes in reactions.mapping keys.
    // Allows operators to write `:thumbsup: = "OK"` instead of `"👍" = "OK"`.
    config.reactions.mapping = config
        .reactions
        .mapping
        .into_iter()
        .map(|(key, val)| {
            let resolved = if key.starts_with(':') && key.ends_with(':') && key.len() > 2 {
                let shortcode = &key[1..key.len() - 1];
                emojis::get_by_shortcode(shortcode)
                    .map(|e| e.as_str().to_string())
                    .unwrap_or(key)
            } else {
                key
            };
            (resolved, val)
        })
        .collect();

    // If [agentcore] is set and [agent] command was not explicitly provided,
    // synthesize agent config to spawn the bundled agentcore-acp adapter.
    if let Some(ref ac) = config.agentcore {
        // Validate ARN format: arn:aws:bedrock-agentcore:REGION:ACCOUNT:runtime/ID
        let parts: Vec<&str> = ac.runtime_arn.split(':').collect();
        anyhow::ensure!(
            parts.len() >= 6
                && parts[0] == "arn"
                && parts[2] == "bedrock-agentcore"
                && !parts[3].is_empty()
                && parts[5].starts_with("runtime/"),
            "agentcore.runtime_arn is not a valid AgentCore Runtime ARN \
             (expected arn:aws:bedrock-agentcore:REGION:ACCOUNT:runtime/ID, got \"{}\")",
            ac.runtime_arn
        );

        if !config.agent.command_explicit {
            // Use native Rust bridge (agentcore feature) or fall back to Python adapter
            #[cfg(feature = "agentcore")]
            let (cmd, args) = {
                let self_exe = std::env::current_exe()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "openab".to_string());
                (
                    self_exe,
                    vec![
                        "agentcore-bridge".into(),
                        "--runtime-arn".into(),
                        ac.runtime_arn.clone(),
                        "--region".into(),
                        ac.region(),
                        "--command".into(),
                        ac.shell_command.clone(),
                    ],
                )
            };
            #[cfg(not(feature = "agentcore"))]
            let (cmd, args) = (
                "uv".to_string(),
                vec![
                    "run".into(),
                    "--script".into(),
                    "/opt/agentcore/acp/agentcore_acp.py".into(),
                    "--runtime-arn".into(),
                    ac.runtime_arn.clone(),
                    "--region".into(),
                    ac.region(),
                    "--cancel-strategy".into(),
                    ac.cancel_strategy.to_string(),
                ],
            );
            config.agent = AgentConfig {
                command: cmd,
                args,
                working_dir: config.agent.working_dir.clone(),
                env: config.agent.env.clone(),
                inherit_env: config.agent.inherit_env.clone(),
                command_explicit: true, // synthesized counts as explicit
            };
        }
    }

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
    anyhow::ensure!(
        config.pool.liveness_check_secs > 0,
        "pool.liveness_check_secs must be > 0 (zero would spin the recv loop)"
    );

    Ok(config)
}

// ---------------------------------------------------------------------------
// Ambient Mode configuration
// ---------------------------------------------------------------------------

/// Top-level `[ambient]` configuration for passive channel listening.
///
/// NOTE: ADR #1211 originally specified `[discord.ambient]`. The implementation
/// uses top-level `[ambient]` with nested `[ambient.discord]` to allow future
/// multi-platform ambient support without restructuring config.
#[derive(Debug, Clone, Deserialize)]
pub struct AmbientConfig {
    /// Master switch (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Time-based flush trigger in seconds (±20% jitter applied). Default: 60.
    #[serde(default = "default_flush_interval_seconds")]
    pub flush_interval_seconds: u64,
    /// Count-based flush trigger. Default: 10.
    #[serde(default = "default_flush_max_messages")]
    pub flush_max_messages: usize,
    /// Safety cap — force flush at this count even if timer hasn't expired.
    /// Only relevant when `flush_max_messages` is set very high or disabled. Default: 50.
    #[serde(default = "default_flush_hard_cap")]
    pub flush_hard_cap: usize,
    /// Historical messages fetched via Discord API before the batch. Default: 20.
    /// NOTE: Not yet implemented (v2 follow-up). Parsed but not used at runtime.
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    /// Max simultaneous LLM calls across all ambient channels. Default: 3.
    #[serde(default = "default_max_concurrent_flushes")]
    pub max_concurrent_flushes: usize,
    /// Safety timeout (seconds) — auto-reset flushing flag if exceeded. Default: 120.
    #[serde(default = "default_flush_timeout_seconds")]
    pub flush_timeout_seconds: u64,
    /// Path to a custom instructions file for the ambient system prompt.
    /// Default: `~/.openab/config/ambient.md`. If the file exists, its content
    /// (up to 2000 characters) replaces the built-in system instruction.
    #[serde(default = "default_instructions_file")]
    pub instructions_file: String,
    /// Ambient session pool configuration.
    #[serde(default)]
    pub pool: AmbientPoolConfig,
    /// Platform-specific ambient settings.
    #[serde(default)]
    pub discord: AmbientDiscordConfig,
    /// Debug mode: when true, [NO_REPLY] responses are sent to the channel
    /// instead of being suppressed, allowing observation of ambient behavior.
    /// ⚠️ WARNING: This exposes the system prompt and buffered messages to the
    /// channel. Only use in private/test channels, never in production.
    #[serde(default)]
    pub debug: bool,
}

impl Default for AmbientConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            flush_interval_seconds: default_flush_interval_seconds(),
            flush_max_messages: default_flush_max_messages(),
            flush_hard_cap: default_flush_hard_cap(),
            context_window: default_context_window(),
            max_concurrent_flushes: default_max_concurrent_flushes(),
            flush_timeout_seconds: default_flush_timeout_seconds(),
            instructions_file: default_instructions_file(),
            pool: AmbientPoolConfig::default(),
            discord: AmbientDiscordConfig::default(),
            debug: false,
        }
    }
}

/// `[ambient.pool]` — dedicated session pool for ambient dispatches.
///
/// NOTE: Pool management is not yet implemented (v2 follow-up). These settings
/// are parsed and validated on startup but not enforced at runtime.
#[derive(Debug, Clone, Deserialize)]
pub struct AmbientPoolConfig {
    /// Max concurrent ambient sessions. Default: 5.
    #[serde(default = "default_ambient_max_sessions")]
    pub max_sessions: usize,
    /// Ambient session inactivity timeout in minutes. Default: 60.
    #[serde(default = "default_ambient_session_ttl_minutes")]
    pub session_ttl_minutes: u64,
    /// Rolling window of retained flush history (cross-flush memory). Default: 3.
    #[serde(default = "default_ambient_context_flushes")]
    pub context_flushes: usize,
}

impl Default for AmbientPoolConfig {
    fn default() -> Self {
        Self {
            max_sessions: default_ambient_max_sessions(),
            session_ttl_minutes: default_ambient_session_ttl_minutes(),
            context_flushes: default_ambient_context_flushes(),
        }
    }
}

/// `[ambient.discord]` — Discord-specific ambient settings.
#[derive(Debug, Clone, Deserialize)]
pub struct AmbientDiscordConfig {
    /// Explicit channel allowlist. Required — empty means ambient is disabled.
    #[serde(default)]
    pub channels: Vec<String>,
    /// Whether other bots' messages enter the ambient buffer. Default: true.
    #[serde(default = "default_true")]
    pub allow_bot_messages: bool,
}

impl Default for AmbientDiscordConfig {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            allow_bot_messages: true,
        }
    }
}

fn default_flush_interval_seconds() -> u64 {
    60
}
fn default_flush_max_messages() -> usize {
    10
}
fn default_flush_hard_cap() -> usize {
    50
}
fn default_context_window() -> usize {
    20
}
fn default_max_concurrent_flushes() -> usize {
    3
}
fn default_flush_timeout_seconds() -> u64 {
    120
}
fn default_instructions_file() -> String {
    "~/.openab/config/ambient.md".to_string()
}
fn default_ambient_max_sessions() -> usize {
    5
}
fn default_ambient_session_ttl_minutes() -> u64 {
    60
}
fn default_ambient_context_flushes() -> usize {
    3
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn telegram_resolve_all_scenarios() {
        // Single serialized test for all TelegramConfig::resolve() scenarios
        // that touch TELEGRAM_* env vars. Consolidated to avoid race conditions
        // under Rust's default parallel test execution (std::env is process-global).

        // --- Clear all TELEGRAM_* env vars ---
        for k in [
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_SECRET_TOKEN",
            "TELEGRAM_TRUSTED_SOURCE_ONLY",
            "TELEGRAM_RICH_MESSAGES",
            "TELEGRAM_STREAMING",
            "TELEGRAM_WEBHOOK_PATH",
        ] {
            std::env::remove_var(k);
        }

        // --- Scenario 1: Config values win over env ---
        std::env::set_var("TELEGRAM_BOT_TOKEN", "env-token");
        let cfg = TelegramConfig {
            bot_token: Some("cfg-token".into()),
            secret_token: Some("cfg-secret".into()),
            trusted_source_only: Some(true),
            rich_messages: Some(false),
            streaming: Some(true),
            webhook_path: Some("/custom/tg".into()),
        };
        let r = cfg.resolve();
        assert_eq!(r.bot_token.as_deref(), Some("cfg-token"));
        assert_eq!(r.secret_token.as_deref(), Some("cfg-secret"));
        assert!(r.trusted_source_only);
        assert!(!r.rich_messages);
        assert_eq!(r.streaming, Some(true));
        assert_eq!(r.webhook_path, "/custom/tg");
        std::env::remove_var("TELEGRAM_BOT_TOKEN");

        // --- Scenario 2: All unset → built-in defaults ---
        for k in [
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_SECRET_TOKEN",
            "TELEGRAM_TRUSTED_SOURCE_ONLY",
            "TELEGRAM_RICH_MESSAGES",
            "TELEGRAM_STREAMING",
            "TELEGRAM_WEBHOOK_PATH",
        ] {
            std::env::remove_var(k);
        }

        let r = TelegramConfig::default().resolve();
        assert_eq!(r.bot_token, None);
        assert_eq!(r.secret_token, None);
        assert!(!r.trusted_source_only); // default false
        assert!(r.rich_messages); // default true
        assert_eq!(r.streaming, None);
        assert_eq!(r.webhook_path, "/webhook/telegram");

        // --- Scenario 3: Env set, config unset → env values used (legacy semantics) ---
        std::env::set_var("TELEGRAM_BOT_TOKEN", "env-token");
        std::env::set_var("TELEGRAM_SECRET_TOKEN", "env-secret");
        std::env::set_var("TELEGRAM_TRUSTED_SOURCE_ONLY", "true");
        std::env::set_var("TELEGRAM_RICH_MESSAGES", "false");
        std::env::set_var("TELEGRAM_STREAMING", "1");
        std::env::set_var("TELEGRAM_WEBHOOK_PATH", "/env/tg");

        let r = TelegramConfig::default().resolve();
        assert_eq!(r.bot_token.as_deref(), Some("env-token"));
        assert_eq!(r.secret_token.as_deref(), Some("env-secret"));
        assert!(r.trusted_source_only);
        assert!(!r.rich_messages); // "false" → false
        assert_eq!(r.streaming, Some(true)); // "1" → true
        assert_eq!(r.webhook_path, "/env/tg");

        // --- Scenario 4: RICH_MESSAGES legacy semantics ---
        std::env::set_var("TELEGRAM_RICH_MESSAGES", "0");
        assert!(!TelegramConfig::default().resolve().rich_messages);
        std::env::set_var("TELEGRAM_RICH_MESSAGES", "yes");
        assert!(TelegramConfig::default().resolve().rich_messages);

        // --- Scenario 5: STREAMING "false" → Some(false) ---
        std::env::set_var("TELEGRAM_STREAMING", "false");
        assert_eq!(TelegramConfig::default().resolve().streaming, Some(false));

        // --- Scenario 6: Empty-string expansion edge case ---
        // When `${}` expands to "" (env var unset at parse time), resolve()
        // must treat it as absent and fall through to env fallback.
        std::env::set_var("TELEGRAM_BOT_TOKEN", "real-token");
        std::env::set_var("TELEGRAM_SECRET_TOKEN", "real-secret");
        std::env::remove_var("TELEGRAM_WEBHOOK_PATH");

        let cfg = TelegramConfig {
            bot_token: Some("".into()),       // simulates ${UNSET_VAR} → ""
            secret_token: Some("".into()),
            webhook_path: Some("".into()),
            ..Default::default()
        };
        let r = cfg.resolve();
        assert_eq!(r.bot_token.as_deref(), Some("real-token"));
        assert_eq!(r.secret_token.as_deref(), Some("real-secret"));
        assert_eq!(r.webhook_path, "/webhook/telegram"); // env not set → default

        // --- Cleanup ---
        for k in [
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_SECRET_TOKEN",
            "TELEGRAM_TRUSTED_SOURCE_ONLY",
            "TELEGRAM_RICH_MESSAGES",
            "TELEGRAM_STREAMING",
            "TELEGRAM_WEBHOOK_PATH",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn telegram_section_parses_from_toml() {
        let toml_str = r#"
[discord]
bot_token = "x"

[telegram]
bot_token = "tg-tok"
secret_token = "tg-sec"
trusted_source_only = true
rich_messages = false
streaming = true
webhook_path = "/hook/tg"
"#;
        let cfg = parse_config_str(toml_str, "test").unwrap();
        let tg = cfg.telegram.expect("telegram section");
        assert_eq!(tg.bot_token.as_deref(), Some("tg-tok"));
        assert_eq!(tg.secret_token.as_deref(), Some("tg-sec"));
        assert_eq!(tg.trusted_source_only, Some(true));
        assert_eq!(tg.rich_messages, Some(false));
        assert_eq!(tg.streaming, Some(true));
        assert_eq!(tg.webhook_path.as_deref(), Some("/hook/tg"));
    }

    #[test]
    fn hooks_any_configured_false_when_empty() {
        let h = HooksConfig::default();
        assert!(!h.any_configured());
        // Empty pre_seed sources don't count as configured
        let h2 = HooksConfig {
            pre_seed: Some(PreSeedConfig {
                sources: vec![],
                target: None,
                region: None,
                endpoint_url: None,
                max_bytes: default_max_zip_bytes(),
                timeout_seconds: default_pre_seed_timeout(),
                on_failure: OnFailure::default(),
            }),
            pre_boot: None,
            pre_shutdown: None,
        };
        assert!(!h2.any_configured());
    }

    fn sample_hook() -> HookConfig {
        HookConfig {
            script: None,
            inline: Some("echo hi".to_string()),
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::default(),
        }
    }

    #[test]
    fn hooks_any_configured_true_with_pre_boot() {
        let h = HooksConfig {
            pre_seed: None,
            pre_boot: Some(sample_hook()),
            pre_shutdown: None,
        };
        assert!(h.any_configured());
    }

    #[test]
    fn ensure_platform_supported_ok_when_no_hooks() {
        // No hooks configured → always Ok regardless of platform
        assert!(HooksConfig::default().ensure_platform_supported().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_platform_supported_ok_on_unix_with_hooks() {
        let h = HooksConfig {
            pre_seed: None,
            pre_boot: Some(sample_hook()),
            pre_shutdown: None,
        };
        assert!(h.ensure_platform_supported().is_ok());
    }

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
    fn parse_s3_uri_splits_bucket_and_key() {
        let (bucket, key) = parse_s3_uri("s3://my-bucket/path/to/config.toml").unwrap();
        assert_eq!(bucket, "my-bucket");
        assert_eq!(key, "path/to/config.toml");
    }

    #[test]
    fn parse_s3_uri_handles_single_segment_key() {
        let (bucket, key) = parse_s3_uri("s3://bkt/config.toml").unwrap();
        assert_eq!(bucket, "bkt");
        assert_eq!(key, "config.toml");
    }

    #[test]
    fn parse_s3_uri_rejects_wrong_scheme() {
        assert!(parse_s3_uri("https://example.com/config.toml").is_err());
        assert!(parse_s3_uri("config.toml").is_err());
    }

    #[test]
    fn parse_s3_uri_rejects_missing_key() {
        // no '/' after bucket
        assert!(parse_s3_uri("s3://only-bucket").is_err());
        // empty key
        assert!(parse_s3_uri("s3://bucket/").is_err());
        // empty bucket
        assert!(parse_s3_uri("s3:///key").is_err());
    }

    #[test]
    fn finalize_config_bytes_accepts_at_limit() {
        let at_limit = vec![b'a'; MAX_CONFIG_BYTES];
        assert!(finalize_config_bytes(&at_limit, "test").is_ok());
    }

    #[test]
    fn finalize_config_bytes_rejects_oversize() {
        let oversize = vec![b'a'; MAX_CONFIG_BYTES + 1];
        let err = finalize_config_bytes(&oversize, "test").unwrap_err();
        assert!(err.to_string().contains("exceeds 1 MiB limit"));
    }

    #[test]
    fn finalize_config_bytes_rejects_invalid_utf8() {
        // 0xFF is never valid in UTF-8
        let bad = [0xff, 0xfe, 0xfd];
        let err = finalize_config_bytes(&bad, "test").unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn finalize_config_bytes_expands_env() {
        std::env::set_var("AB_FINALIZE_TEST", "world");
        let out = finalize_config_bytes(b"hello=${AB_FINALIZE_TEST}", "test").unwrap();
        assert_eq!(out, "hello=world");
        std::env::remove_var("AB_FINALIZE_TEST");
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

    #[test]
    fn parse_secrets_config() {
        let toml = r#"
[discord]
bot_token = "${secrets.discord_token}"

[agent]
command = "echo"

[secrets.refs]
discord_token = "aws-sm://openab/prod#discord_bot_token"
github_pat = "exec:///home/agent/.local/bin/get-secret.sh vault/openab github_pat"

[secrets.aws]
region = "ap-northeast-1"
endpoint_url = "http://localhost:4566"

[secrets.exec]
timeout_seconds = 15
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(cfg.secrets.refs.len(), 2);
        assert_eq!(
            cfg.secrets.refs.get("discord_token").unwrap(),
            "aws-sm://openab/prod#discord_bot_token"
        );
        assert_eq!(
            cfg.secrets.refs.get("github_pat").unwrap(),
            "exec:///home/agent/.local/bin/get-secret.sh vault/openab github_pat"
        );
        assert_eq!(cfg.secrets.aws.region.as_deref(), Some("ap-northeast-1"));
        assert_eq!(
            cfg.secrets.aws.endpoint_url.as_deref(),
            Some("http://localhost:4566")
        );
        assert_eq!(cfg.secrets.exec.timeout_seconds, 15);
    }

    #[test]
    fn parse_secrets_config_defaults() {
        let toml = r#"
[discord]
bot_token = "test"

[agent]
command = "echo"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert!(cfg.secrets.refs.is_empty());
        assert!(cfg.secrets.aws.region.is_none());
        assert!(cfg.secrets.aws.endpoint_url.is_none());
        assert_eq!(cfg.secrets.exec.timeout_seconds, 10);
    }

    #[test]
    fn slack_assistant_mode_defaults_true_and_parses_false() {
        let cfg: SlackConfig = toml::from_str("bot_token = \"x\"\napp_token = \"y\"\n").unwrap();
        assert!(cfg.assistant_mode, "assistant_mode must default to true");

        let cfg2: SlackConfig =
            toml::from_str("bot_token = \"x\"\napp_token = \"y\"\nassistant_mode = false\n")
                .unwrap();
        assert!(!cfg2.assistant_mode);
    }

    #[test]
    fn agentcore_config_synthesizes_agent_command() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/my-agent"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        #[cfg(feature = "agentcore")]
        {
            // With agentcore feature, spawns self with agentcore-bridge subcommand
            assert!(cfg.agent.args.contains(&"agentcore-bridge".to_string()));
        }
        #[cfg(not(feature = "agentcore"))]
        {
            assert_eq!(cfg.agent.command, "uv");
        }
        assert!(cfg.agent.args.contains(&"--runtime-arn".to_string()));
        assert!(cfg
            .agent
            .args
            .contains(&"arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/my-agent".to_string()));
    }

    #[test]
    fn agentcore_config_does_not_override_explicit_agent() {
        let toml = r#"
[discord]
bot_token = "t"

[agent]
command = "my-custom-agent"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/my-agent"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert_eq!(cfg.agent.command, "my-custom-agent");
    }

    #[test]
    fn agentcore_config_defaults() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/test"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        let ac = cfg.agentcore.unwrap();
        assert_eq!(ac.region(), "us-east-1");
        assert_eq!(ac.cancel_strategy, AgentCoreCancelStrategy::Stop);
    }

    #[test]
    fn agentcore_rejects_invalid_arn() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "not-a-valid-arn"
"#;
        let err = parse_config(toml, "test").unwrap_err();
        assert!(err.to_string().contains("not a valid AgentCore Runtime ARN"));
    }

    #[test]
    fn agentcore_rejects_arn_wrong_service() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:s3:us-east-1:123456789012:bucket/my-bucket"
"#;
        let err = parse_config(toml, "test").unwrap_err();
        assert!(err.to_string().contains("not a valid AgentCore Runtime ARN"));
    }

    #[test]
    fn agentcore_rejects_arn_missing_runtime_prefix() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:agent/my-agent"
"#;
        let err = parse_config(toml, "test").unwrap_err();
        assert!(err.to_string().contains("not a valid AgentCore Runtime ARN"));
    }

    #[test]
    fn agentcore_rejects_invalid_cancel_strategy() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/test"
cancel_strategy = "stopp"
"#;
        let err = parse_config(toml, "test").unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn agentcore_extracts_region_from_arn() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:ap-northeast-1:123456789012:runtime/tokyo-agent"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        assert!(cfg.agent.args.contains(&"ap-northeast-1".to_string()));
    }

    #[test]
    fn agentcore_cancel_strategy_noop() {
        let toml = r#"
[discord]
bot_token = "t"

[agentcore]
runtime_arn = "arn:aws:bedrock-agentcore:us-east-1:123456789012:runtime/test"
cancel_strategy = "noop"
"#;
        let cfg = parse_config(toml, "test").unwrap();
        let ac = cfg.agentcore.unwrap();
        assert_eq!(ac.cancel_strategy, AgentCoreCancelStrategy::Noop);
    }
}

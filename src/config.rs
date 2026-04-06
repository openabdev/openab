use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub pool: PoolConfig,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ChatMode {
    /// Personal mode (original): any message in #general/All creates a new topic.
    /// Inside topics, all messages get a reply directly — no @mention needed.
    Personal,
    /// Team mode: only `!kiro <prompt>` creates a new topic (restricted to topic_creator_id).
    /// Inside topics, Kiro listens silently to all messages but only replies when @mentioned.
    Team,
}

impl Default for ChatMode {
    fn default() -> Self { ChatMode::Personal }
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_users: Vec<i64>,
    /// If set, only this user ID can trigger `!kiro` topic creation (team mode).
    pub topic_creator_id: Option<i64>,
    /// `personal` (default) or `team`. See ChatMode for details.
    #[serde(default)]
    pub mode: ChatMode,
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
}

fn default_working_dir() -> String { "/tmp".into() }
fn default_max_sessions() -> usize { 10 }

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_sessions: default_max_sessions() }
    }
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
    let expanded = expand_env_vars(&raw);
    let config: Config = toml::from_str(&expanded)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(config)
}

//! Config generation and TOML serialization for the setup wizard.

/// Mask bot token in config output for preview
pub fn mask_bot_token(config: &str) -> String {
    config
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("bot_token") {
                "bot_token = \"***\"".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(serde::Serialize)]
pub(crate) struct ConfigToml {
    discord: DiscordConfigToml,
    agent: AgentConfigToml,
    pool: PoolConfigToml,
    reactions: ReactionsConfigToml,
}

#[derive(serde::Serialize)]
struct DiscordConfigToml {
    bot_token: String,
    allowed_channels: Vec<String>,
}

#[derive(serde::Serialize)]
struct AgentConfigToml {
    command: String,
    args: Vec<String>,
    working_dir: String,
}

#[derive(serde::Serialize)]
struct PoolConfigToml {
    max_sessions: usize,
    session_ttl_hours: u64,
}

#[derive(serde::Serialize)]
struct ReactionsConfigToml {
    enabled: bool,
    remove_after_reply: bool,
    emojis: EmojisToml,
    timing: TimingToml,
}

#[derive(serde::Serialize)]
struct EmojisToml {
    queued: String,
    thinking: String,
    tool: String,
    coding: String,
    web: String,
    done: String,
    error: String,
}

#[derive(serde::Serialize)]
struct TimingToml {
    debounce_ms: u64,
    stall_soft_ms: u64,
    stall_hard_ms: u64,
    done_hold_ms: u64,
    error_hold_ms: u64,
}

pub fn generate_config(
    bot_token: &str,
    agent_command: &str,
    channel_ids: Vec<String>,
    working_dir: &str,
    max_sessions: usize,
    session_ttl_hours: u64,
) -> String {
    let config = ConfigToml {
        discord: DiscordConfigToml {
            bot_token: bot_token.to_string(),
            allowed_channels: channel_ids,
        },
        agent: {
            let (command, args): (&str, Vec<String>) = match agent_command {
                "kiro" => ("kiro-cli", vec!["acp".into(), "--trust-all-tools".into()]),
                "claude" => ("claude-agent-acp", vec![]),
                "codex" => ("codex-acp", vec![]),
                "gemini" => ("gemini", vec!["--acp".into()]),
                other => (other, vec![]),
            };
            AgentConfigToml {
                command: command.to_string(),
                args,
                working_dir: working_dir.to_string(),
            }
        },
        pool: PoolConfigToml {
            max_sessions,
            session_ttl_hours,
        },
        reactions: ReactionsConfigToml {
            enabled: true,
            remove_after_reply: false,
            emojis: EmojisToml {
                queued: "👀".into(),
                thinking: "🤔".into(),
                tool: "🔥".into(),
                coding: "👨💻".into(),
                web: "⚡".into(),
                done: "🆗".into(),
                error: "😱".into(),
            },
            timing: TimingToml {
                debounce_ms: 700,
                stall_soft_ms: 10_000,
                stall_hard_ms: 30_000,
                done_hold_ms: 1_500,
                error_hold_ms: 2_500,
            },
        },
    };
    toml::to_string_pretty(&config).expect("TOML serialization failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_config_contains_sections() {
        let config = generate_config(
            "my_token",
            "claude",
            vec!["123".to_string()],
            "/home/agent",
            10,
            24,
        );
        assert!(config.contains("[discord]"));
        assert!(config.contains("[agent]"));
        assert!(config.contains("[pool]"));
        assert!(config.contains("[reactions]"));
        assert!(config.contains("[reactions.emojis]"));
        assert!(config.contains("[reactions.timing]"));
    }

    #[test]
    fn test_generate_config_kiro_working_dir() {
        let config = generate_config("tok", "kiro", vec!["ch".to_string()], "/home/agent", 10, 24);
        assert!(config.contains(r#"working_dir = "/home/agent""#));
        assert!(config.contains("acp"));
        assert!(config.contains("--trust-all-tools"));
    }
}

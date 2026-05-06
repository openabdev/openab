//! Input validation functions for the setup wizard.

/// Validate bot token format using allowlist (a-zA-Z0-9-./_)
pub fn validate_bot_token(token: &str) -> anyhow::Result<()> {
    if token.is_empty() {
        anyhow::bail!("Token cannot be empty");
    }
    if !token.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '-'
            || c == '.'
            || c == '_'
            || c == '/'
            || c == '*'
            || c == '='
    }) {
        anyhow::bail!(
            "Token must only contain ASCII letters, numbers, dash, period, underscore, slash, or equals"
        );
    }
    Ok(())
}

/// Validate agent command
#[cfg(test)]
pub fn validate_agent_command(cmd: &str) -> anyhow::Result<()> {
    let valid = ["kiro", "claude", "codex", "gemini"];
    if !valid.contains(&cmd) {
        anyhow::bail!("Agent must be one of: {}", valid.join(", "));
    }
    Ok(())
}

/// Validate channel ID is numeric
pub fn validate_channel_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("Channel ID cannot be empty");
    }
    if !id.chars().all(|c| c.is_ascii_digit()) {
        anyhow::bail!("Channel ID must be numeric only");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_bot_token_ok() {
        assert!(validate_bot_token("simple_token").is_ok());
        assert!(validate_bot_token("token.with-dashes_123").is_ok());
        assert!(validate_bot_token("***/efgh").is_ok());
    }

    #[test]
    fn test_validate_bot_token_reject_invalid() {
        assert!(validate_bot_token("").is_err());
        assert!(validate_bot_token("token\nnewline").is_err());
        assert!(validate_bot_token("token\ttab").is_err());
        assert!(validate_bot_token("token with space").is_err());
    }

    #[test]
    fn test_validate_agent_command() {
        for agent in &["kiro", "claude", "codex", "gemini"] {
            assert!(validate_agent_command(agent).is_ok());
        }
        assert!(validate_agent_command("invalid").is_err());
    }

    #[test]
    fn test_validate_channel_id() {
        assert!(validate_channel_id("1492329565824094370").is_ok());
        assert!(validate_channel_id("").is_err());
        assert!(validate_channel_id("abc123").is_err());
    }
}

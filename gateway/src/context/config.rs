pub const DEFAULT_CONTEXT_TTL_HOURS: u64 = 24;
pub const DEFAULT_CONTEXT_MAX_MESSAGES: usize = 50;
pub const DEFAULT_CONTEXT_MAX_CHARS: usize = 8_000;

#[derive(Clone, Debug)]
pub struct ContextConfig {
    pub enabled: bool,
    pub ttl_secs: u64,
    pub max_messages: usize,
    pub max_chars: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: DEFAULT_CONTEXT_TTL_HOURS * 60 * 60,
            max_messages: DEFAULT_CONTEXT_MAX_MESSAGES,
            max_chars: DEFAULT_CONTEXT_MAX_CHARS,
        }
    }
}

impl ContextConfig {
    pub fn from_env_with_prefixes(prefixes: &[&str]) -> Self {
        let defaults = Self::default();
        let ttl_hours =
            read_positive_env_u64(prefixes, "CONTEXT_TTL_HOURS", DEFAULT_CONTEXT_TTL_HOURS);

        Self {
            enabled: read_bool_env(prefixes, "CONTEXT_ENABLED", defaults.enabled),
            ttl_secs: ttl_hours.saturating_mul(60 * 60),
            max_messages: read_positive_env_usize(
                prefixes,
                "CONTEXT_MAX_MESSAGES",
                defaults.max_messages,
            ),
            max_chars: read_positive_env_usize(prefixes, "CONTEXT_MAX_CHARS", defaults.max_chars),
        }
    }
}

fn read_bool_env(prefixes: &[&str], suffix: &str, default: bool) -> bool {
    env_names(prefixes, suffix)
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        })
        .unwrap_or(default)
}

fn read_positive_env_u64(prefixes: &[&str], suffix: &str, default: u64) -> u64 {
    env_names(prefixes, suffix)
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
        })
        .unwrap_or(default)
}

fn read_positive_env_usize(prefixes: &[&str], suffix: &str, default: usize) -> usize {
    env_names(prefixes, suffix)
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
        })
        .unwrap_or(default)
}

fn env_names(prefixes: &[&str], suffix: &str) -> Vec<String> {
    prefixes
        .iter()
        .map(|prefix| format!("{prefix}_{suffix}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_context_config_is_disabled_and_bounded() {
        let config = ContextConfig::default();

        assert!(!config.enabled);
        assert_eq!(config.ttl_secs, 24 * 60 * 60);
        assert_eq!(config.max_messages, 50);
        assert_eq!(config.max_chars, 8_000);
    }
}

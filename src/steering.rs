use crate::config::{SteeringConfig, SteeringFallback, SteeringMode};

/// Check whether a raw prompt text qualifies as a steering message under
/// the given configuration. Returns `Some(stripped_text)` when steering
/// applies (prefix stripped), or `None` when normal queueing should proceed.
///
/// Rules:
/// - `SteeringMode::Off` → never steering.
/// - `SteeringMode::Prefix` → steering only if text starts with the configured prefix.
/// - `SteeringMode::Implicit` → always steering (caller must verify session is busy).
pub fn detect_steering(prompt: &str, config: &SteeringConfig) -> Option<String> {
    if !config.enabled {
        return None;
    }
    match config.mode {
        SteeringMode::Off => None,
        SteeringMode::Prefix => {
            let trimmed = prompt.trim_start();
            if let Some(rest) = trimmed.strip_prefix(&config.prefix) {
                let stripped = rest.trim_start();
                if stripped.is_empty() {
                    None
                } else {
                    Some(stripped.to_string())
                }
            } else {
                None
            }
        }
        SteeringMode::Implicit => {
            let trimmed = prompt.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}

/// Build a user-facing error message when steering injection fails and
/// the configured fallback is `SteeringFallback::Error`.
pub fn format_steering_error(err: &str) -> String {
    format!("⚠️ Steering failed: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix_config() -> SteeringConfig {
        SteeringConfig {
            enabled: true,
            prefix: "!!".into(),
            mode: SteeringMode::Prefix,
            fallback: SteeringFallback::Queue,
        }
    }

    fn implicit_config() -> SteeringConfig {
        SteeringConfig {
            enabled: true,
            prefix: "!!".into(),
            mode: SteeringMode::Implicit,
            fallback: SteeringFallback::Queue,
        }
    }

    #[test]
    fn disabled_mode_never_steers() {
        let cfg = SteeringConfig {
            enabled: false,
            ..prefix_config()
        };
        assert_eq!(detect_steering("!! stop", &cfg), None);
    }

    #[test]
    fn prefix_detects_and_strips() {
        let cfg = prefix_config();
        assert_eq!(
            detect_steering("!! stop — use staging", &cfg),
            Some("stop — use staging".into())
        );
    }

    #[test]
    fn prefix_requires_exact_prefix() {
        let cfg = prefix_config();
        assert_eq!(detect_steering("! stop", &cfg), None);
        assert_eq!(detect_steering("!!! stop", &cfg), None);
    }

    #[test]
    fn prefix_strips_leading_whitespace() {
        let cfg = prefix_config();
        assert_eq!(
            detect_steering("  !!   stop now", &cfg),
            Some("stop now".into())
        );
    }

    #[test]
    fn prefix_empty_after_stripping_is_none() {
        let cfg = prefix_config();
        assert_eq!(detect_steering("!!", &cfg), None);
        assert_eq!(detect_steering("!!   ", &cfg), None);
    }

    #[test]
    fn implicit_mode_always_matches_non_empty() {
        let cfg = implicit_config();
        assert_eq!(
            detect_steering("just a normal message", &cfg),
            Some("just a normal message".into())
        );
    }

    #[test]
    fn implicit_mode_rejects_empty() {
        let cfg = implicit_config();
        assert_eq!(detect_steering("", &cfg), None);
        assert_eq!(detect_steering("   ", &cfg), None);
    }

    #[test]
    fn custom_prefix_works() {
        let cfg = SteeringConfig {
            enabled: true,
            prefix: "/steer".into(),
            mode: SteeringMode::Prefix,
            fallback: SteeringFallback::Queue,
        };
        assert_eq!(
            detect_steering("/steer revert that change", &cfg),
            Some("revert that change".into())
        );
        assert_eq!(detect_steering("!! revert", &cfg), None);
    }
}

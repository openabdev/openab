//! Centralized agent config (ADR §5.5 — declarative defaults; secrets stay in
//! `auth.json`). A small JSON file next to `auth.json` that supplies the default
//! provider/model and a few params, so a deployment can declare them in a file
//! instead of only via env vars.
//!
//! Precedence is **env-over-config**: `OPENAB_AGENT_MODEL` / `OPENAB_AGENT_MAX_TOKENS`
//! still win, and this file is the declarative default for runs that don't set
//! them (a pod's injected env stays authoritative over a baked config). The
//! resolution chain lives in `llm.rs` (`anthropic_model` / `anthropic_max_tokens`
//! / `resolve_provider_choice`).
//!
//! Unknown keys are tolerated (forward-compat: `providers`/`small_model`/… can
//! land later without breaking older binaries). Secrets never live here.

use serde::Deserialize;
use std::path::PathBuf;

/// Parsed `config.json`. Every field is optional — a missing file is an empty
/// config, and any field falls back to env/built-in default downstream.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Default model as a single `provider/model` string (see `ModelRef`).
    pub model: Option<String>,
    /// Default max output tokens.
    pub max_tokens: Option<u32>,
    /// Turn-envelope schema to produce, e.g. `"openab.turn.v1"`. Absent — the
    /// default — leaves the agent answering in plain text exactly as before.
    /// Set it to make the agent reply through the `reply` tool so the broker
    /// can deliver one chat message per bubble (see `crate::turn_envelope`).
    /// Env override: `OPENAB_AGENT_TURN_ENVELOPE`.
    pub turn_envelope: Option<String>,
}

impl AgentConfig {
    /// `config.json` next to `auth.json`. `OPENAB_CONFIG_PATH` overrides the
    /// whole path (ops injection / tests).
    pub fn path() -> PathBuf {
        if let Ok(p) = std::env::var("OPENAB_CONFIG_PATH") {
            return PathBuf::from(p);
        }
        crate::auth::auth_path().with_file_name("config.json")
    }

    /// Parse config JSON. Pure (unit-testable).
    pub fn parse(data: &str) -> anyhow::Result<Self> {
        serde_json::from_str(data).map_err(Into::into)
    }

    /// Load + parse the config file. A missing file is an empty config (not an
    /// error); a present-but-malformed file IS an error so a typo is visible
    /// rather than silently dropped.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                Self::parse(&s).map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("reading {}: {e}", path.display())),
        }
    }

    /// Load, or an empty config (logging a warning) when the file is malformed —
    /// a typo'd config must not crash the agent, but it has to be visible. Used
    /// by the resolution path, which then falls through to env/built-in defaults.
    pub fn load_or_default() -> Self {
        match Self::load() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("ignoring config: {e}");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_known_fields() {
        let c = AgentConfig::parse(r#"{"model":"anthropic/claude-sonnet-4-6","max_tokens":4096}"#)
            .unwrap();
        assert_eq!(c.model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
        assert_eq!(c.max_tokens, Some(4096));
    }

    #[test]
    fn empty_object_is_all_none() {
        let c = AgentConfig::parse("{}").unwrap();
        assert_eq!(c.model, None);
        assert_eq!(c.max_tokens, None);
        assert_eq!(c.turn_envelope, None);
    }

    #[test]
    fn parse_reads_turn_envelope() {
        let c = AgentConfig::parse(r#"{"turn_envelope":"openab.turn.v1"}"#).unwrap();
        assert_eq!(c.turn_envelope.as_deref(), Some("openab.turn.v1"));
    }

    #[test]
    fn unknown_keys_tolerated_for_forward_compat() {
        // `providers` / future keys must not break an older binary.
        let c =
            AgentConfig::parse(r#"{"model":"anthropic/x","providers":{"anthropic":{}}}"#).unwrap();
        assert_eq!(c.model.as_deref(), Some("anthropic/x"));
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(AgentConfig::parse("{not json").is_err());
        // wrong type for a known field is also a hard error (fail loud)
        assert!(AgentConfig::parse(r#"{"max_tokens":"lots"}"#).is_err());
    }
}

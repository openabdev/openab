//! OAuth provider catalog (ADR §6.2). Wiring into the rmcp Streamable HTTP
//! transport + agent-guided flows (§6.4) lands in subsequent slices; this
//! module is the data layer the login / refresh code will dispatch through.
//!
//! Scopes are stored as `&'static [&'static str]` so callers can join them
//! with the space delimiter the OAuth 2.1 spec mandates without owning a
//! `Vec`. Per-server overrides (`OAuthConfig.scopes`) replace the defaults
//! and pay for a `Vec<String>` at the boundary.

// The §6.4 login slice is the first prod caller — until then, every item
// here is reachable only via the unit tests below, so `cargo clippy
// --features mcp -- -D warnings` would flag them as dead. Module-scope
// allow rather than per-item once that slice lands.
#![allow(dead_code)]

use anyhow::{anyhow, Result};

use super::config::OAuthConfig;

/// Static description of a single OAuth provider — URLs + the loopback
/// redirect the §6.4 browser flow listens on. `default_scopes` is the
/// minimum set the agent will request when `oauth.scopes` is omitted
/// from the server config; per-server overrides win when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub callback: &'static str,
    pub default_scopes: &'static [&'static str],
}

/// Anthropic MCP (claude.ai). Scope list from ADR §6.2 — `org:create_api_key`
/// is the broadest grant; consumers should narrow via per-server overrides.
pub const ANTHROPIC_MCP: ProviderSpec = ProviderSpec {
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://platform.claude.com/v1/oauth/token",
    callback: "http://localhost:53692/callback",
    default_scopes: &[
        "org:create_api_key",
        "user:profile",
        "user:inference",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload",
    ],
};

/// Look up a built-in `ProviderSpec` by config name. Returns `None` for
/// custom providers (handled by §6.3 once `OAuthConfig` grows the URL
/// fields) and for unknown names.
pub fn builtin(name: &str) -> Option<ProviderSpec> {
    match name {
        "anthropic-mcp" => Some(ANTHROPIC_MCP),
        _ => None,
    }
}

/// Resolve a server's `oauth:` block to a `ProviderSpec` plus the effective
/// scope list. `OAuthConfig::scopes`, when non-empty, replaces the spec's
/// defaults entirely — the caller never needs to merge.
///
/// Custom providers (per ADR §6.3) require `OAuthConfig` to grow
/// `authorize_url` / `token_url` fields; until that lands, an `oauth:`
/// block without a known `provider` is an error.
pub fn resolve(cfg: &OAuthConfig) -> Result<(ProviderSpec, Vec<String>)> {
    let provider = cfg
        .provider
        .as_deref()
        .ok_or_else(|| anyhow!("oauth.provider is required (custom providers land in §6.3)"))?;
    let spec = builtin(provider)
        .ok_or_else(|| anyhow!("unknown oauth provider {provider:?} (built-ins: anthropic-mcp)"))?;
    let scopes = if cfg.scopes.is_empty() {
        spec.default_scopes.iter().map(|s| s.to_string()).collect()
    } else {
        cfg.scopes.clone()
    };
    Ok((spec, scopes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_mcp_spec_matches_adr_table() {
        let spec = builtin("anthropic-mcp").expect("anthropic-mcp is built-in");
        assert_eq!(spec.authorize_url, "https://claude.ai/oauth/authorize");
        assert_eq!(spec.token_url, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(spec.callback, "http://localhost:53692/callback");
        assert!(spec.default_scopes.contains(&"user:mcp_servers"));
    }

    #[test]
    fn unknown_provider_returns_none() {
        assert!(builtin("does-not-exist").is_none());
        assert!(builtin("").is_none());
    }

    #[test]
    fn resolve_uses_default_scopes_when_config_omits_them() {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            ..Default::default()
        };
        let (spec, scopes) = resolve(&cfg).unwrap();
        assert_eq!(spec, ANTHROPIC_MCP);
        assert_eq!(scopes.len(), ANTHROPIC_MCP.default_scopes.len());
    }

    #[test]
    fn resolve_uses_config_scopes_when_provided() {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            scopes: vec!["user:profile".to_string(), "user:inference".to_string()],
            ..Default::default()
        };
        let (_, scopes) = resolve(&cfg).unwrap();
        assert_eq!(scopes, vec!["user:profile", "user:inference"]);
    }

    #[test]
    fn resolve_rejects_missing_provider() {
        let cfg = OAuthConfig::default();
        let err = resolve(&cfg).unwrap_err().to_string();
        assert!(err.contains("required"), "got: {err}");
    }

    #[test]
    fn resolve_rejects_unknown_provider() {
        let cfg = OAuthConfig {
            provider: Some("github-copilot".to_string()),
            ..Default::default()
        };
        let err = resolve(&cfg).unwrap_err().to_string();
        assert!(err.contains("unknown oauth provider"), "got: {err}");
    }
}

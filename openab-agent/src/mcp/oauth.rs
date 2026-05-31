//! OAuth provider catalog (ADR §6.2) + custom-provider resolution (§6.3).
//! Wiring into the rmcp Streamable HTTP transport + agent-guided flows
//! (§6.4) lands in subsequent slices; this module is the data layer the
//! login / refresh code will dispatch through.

// The §6.4 login slice is the first prod caller — until then, every item
// here is reachable only via the unit tests below, so `cargo clippy
// --features mcp -- -D warnings` would flag them as dead. Module-scope
// allow rather than per-item once that slice lands.
#![allow(dead_code)]

use anyhow::{anyhow, Result};

use super::config::OAuthConfig;

/// Static description of a single built-in OAuth provider. `default_scopes`
/// is the minimum set the agent will request when `oauth.scopes` is omitted
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
/// custom providers (§6.3) and for unknown names.
pub fn builtin(name: &str) -> Option<ProviderSpec> {
    match name {
        "anthropic-mcp" => Some(ANTHROPIC_MCP),
        _ => None,
    }
}

/// Effective per-server OAuth parameters after resolving the built-in catalog
/// and `OAuthConfig` overrides. `callback` is `None` for custom providers
/// (§6.4 picks a free port at login time); built-ins pin theirs. `client_id`
/// is `None` for built-ins (the per-provider flow code in §6.4 owns it) and
/// optional for custom providers — OAuth 2.1 servers vary on whether public
/// clients must register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: Option<String>,
    pub callback: Option<String>,
    pub device_authorization_endpoint: Option<String>,
    pub scopes: Vec<String>,
}

/// Resolve a server's `oauth:` block. Built-in providers come from
/// `builtin()`; unknown providers fall through to the §6.3 custom path,
/// which requires `authorize_url` + `token_url` on the config.
///
/// `OAuthConfig::scopes`, when non-empty, replaces the spec's defaults
/// entirely — the caller never needs to merge.
pub fn resolve(cfg: &OAuthConfig) -> Result<ResolvedProvider> {
    let provider = cfg
        .provider
        .as_deref()
        .ok_or_else(|| anyhow!("oauth.provider is required"))?;
    if let Some(spec) = builtin(provider) {
        Ok(resolve_builtin(spec, cfg))
    } else {
        resolve_custom(provider, cfg)
    }
}

fn resolve_builtin(spec: ProviderSpec, cfg: &OAuthConfig) -> ResolvedProvider {
    let scopes = if cfg.scopes.is_empty() {
        spec.default_scopes.iter().map(|s| s.to_string()).collect()
    } else {
        cfg.scopes.clone()
    };
    ResolvedProvider {
        authorize_url: spec.authorize_url.to_string(),
        token_url: spec.token_url.to_string(),
        client_id: None,
        callback: Some(spec.callback.to_string()),
        device_authorization_endpoint: None,
        scopes,
    }
}

fn resolve_custom(provider: &str, cfg: &OAuthConfig) -> Result<ResolvedProvider> {
    let authorize_url = cfg.authorize_url.clone().ok_or_else(|| {
        anyhow!("custom oauth provider {provider:?}: oauth.authorize_url is required (ADR §6.3)")
    })?;
    let token_url = cfg.token_url.clone().ok_or_else(|| {
        anyhow!("custom oauth provider {provider:?}: oauth.token_url is required (ADR §6.3)")
    })?;
    Ok(ResolvedProvider {
        authorize_url,
        token_url,
        client_id: cfg.client_id.clone(),
        callback: None,
        device_authorization_endpoint: cfg.device_authorization_endpoint.clone(),
        scopes: cfg.scopes.clone(),
    })
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
    fn resolve_builtin_uses_default_scopes_when_config_omits_them() {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            ..Default::default()
        };
        let r = resolve(&cfg).unwrap();
        assert_eq!(r.authorize_url, ANTHROPIC_MCP.authorize_url);
        assert_eq!(r.callback.as_deref(), Some(ANTHROPIC_MCP.callback));
        assert_eq!(r.scopes.len(), ANTHROPIC_MCP.default_scopes.len());
        assert!(r.client_id.is_none());
        assert!(r.device_authorization_endpoint.is_none());
    }

    #[test]
    fn resolve_builtin_uses_config_scopes_when_provided() {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            scopes: vec!["user:profile".to_string(), "user:inference".to_string()],
            ..Default::default()
        };
        let r = resolve(&cfg).unwrap();
        assert_eq!(r.scopes, vec!["user:profile", "user:inference"]);
    }

    #[test]
    fn resolve_rejects_missing_provider() {
        let err = resolve(&OAuthConfig::default()).unwrap_err().to_string();
        assert!(err.contains("required"), "got: {err}");
    }

    #[test]
    fn resolve_custom_uses_config_urls_and_propagates_device_endpoint() {
        let cfg = OAuthConfig {
            provider: Some("linear".to_string()),
            authorize_url: Some("https://linear.app/oauth/authorize".to_string()),
            token_url: Some("https://api.linear.app/oauth/token".to_string()),
            client_id: Some("client-abc".to_string()),
            device_authorization_endpoint: Some("https://linear.app/oauth/device".to_string()),
            scopes: vec!["read".to_string(), "write".to_string()],
            ..Default::default()
        };
        let r = resolve(&cfg).unwrap();
        assert_eq!(r.authorize_url, "https://linear.app/oauth/authorize");
        assert_eq!(r.token_url, "https://api.linear.app/oauth/token");
        assert_eq!(r.client_id.as_deref(), Some("client-abc"));
        assert_eq!(
            r.device_authorization_endpoint.as_deref(),
            Some("https://linear.app/oauth/device"),
        );
        assert!(
            r.callback.is_none(),
            "custom providers defer callback to login-time port allocation",
        );
        assert_eq!(r.scopes, vec!["read", "write"]);
    }

    #[test]
    fn resolve_custom_minimal_two_urls_only() {
        let cfg = OAuthConfig {
            provider: Some("acme".to_string()),
            authorize_url: Some("https://acme.example/authorize".to_string()),
            token_url: Some("https://acme.example/token".to_string()),
            ..Default::default()
        };
        let r = resolve(&cfg).unwrap();
        assert!(r.client_id.is_none());
        assert!(r.device_authorization_endpoint.is_none());
        assert!(r.callback.is_none());
        assert!(r.scopes.is_empty());
    }

    #[test]
    fn resolve_custom_rejects_missing_authorize_url() {
        let cfg = OAuthConfig {
            provider: Some("custom".to_string()),
            token_url: Some("https://example.com/token".to_string()),
            ..Default::default()
        };
        let err = resolve(&cfg).unwrap_err().to_string();
        assert!(err.contains("authorize_url"), "got: {err}");
        assert!(err.contains("custom"), "got: {err}");
    }

    #[test]
    fn resolve_custom_rejects_missing_token_url() {
        let cfg = OAuthConfig {
            provider: Some("custom".to_string()),
            authorize_url: Some("https://example.com/authorize".to_string()),
            ..Default::default()
        };
        let err = resolve(&cfg).unwrap_err().to_string();
        assert!(err.contains("token_url"), "got: {err}");
    }
}

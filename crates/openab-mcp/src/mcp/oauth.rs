//! OAuth provider catalog (ADR §6.2) + custom-provider resolution (§6.3).
//! Wiring into the rmcp Streamable HTTP transport + agent-guided flows
//! (§6.4) lands in subsequent slices; this module is the data layer the
//! login / refresh code will dispatch through.

use anyhow::{anyhow, Result};

use super::config::OAuthConfig;

/// Static description of a single built-in OAuth provider. `default_scopes`
/// is the minimum set the agent will request when `oauth.scopes` is omitted
/// from the server config; per-server overrides win when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    pub name: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub callback: &'static str,
    pub default_scopes: &'static [&'static str],
}

/// Anthropic MCP (claude.ai). Scope list from ADR §6.2 — `org:create_api_key`
/// is the broadest grant; consumers should narrow via per-server overrides.
pub const ANTHROPIC_MCP: ProviderSpec = ProviderSpec {
    name: "anthropic-mcp",
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

const BUILTINS: &[ProviderSpec] = &[ANTHROPIC_MCP];

/// Look up a built-in `ProviderSpec` by config name. Returns `None` for
/// custom providers (§6.3) and for unknown names.
pub fn builtin(name: &str) -> Option<ProviderSpec> {
    BUILTINS.iter().copied().find(|spec| spec.name == name)
}

/// Resolve a built-in provider's OAuth `client_id`. Mirrors
/// `auth::codex_client_id`'s env-var-override pattern but without a hard-
/// coded default — the Anthropic MCP public client_id isn't yet pinned in
/// this repo, so requiring the env var fails fast with a useful error
/// rather than silently dialing with a placeholder. Replace with a
/// hard-coded default once a real value is published.
pub fn builtin_client_id(provider: &str) -> Result<String> {
    let env_var = match provider {
        "anthropic-mcp" => "OPENAB_MCP_ANTHROPIC_CLIENT_ID",
        other => {
            return Err(anyhow!(
                "no built-in client_id mapping for provider {other:?}"
            ));
        }
    };
    std::env::var(env_var).map_err(|_| {
        anyhow!(
            "built-in provider {provider:?} requires env var {env_var} \
             (client_id of the provider's OAuth app)"
        )
    })
}

/// Effective per-server OAuth parameters after resolving the built-in catalog
/// and `OAuthConfig` overrides.
///
/// The two variants encode invariants that an `Option`-heavy struct couldn't:
/// built-ins always pin a `callback` (their PKCE port is hard-coded in the
/// provider's app registration) and never carry a `client_id` (the §6.4 flow
/// code owns it, mirroring `auth.rs::codex_client_id()`). Custom providers
/// flip both: §6.4 allocates a free port at login time, and `client_id`
/// comes from config (OAuth 2.1 public clients vary on registration).
///
/// `device_authorization_endpoint` only appears on `Custom` — adding device
/// support for a built-in provider is a `ProviderSpec` schema change, not a
/// config flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProvider {
    Builtin {
        provider_name: &'static str,
        authorize_url: &'static str,
        token_url: &'static str,
        callback: &'static str,
        scopes: Vec<String>,
    },
    Custom {
        provider_name: String,
        authorize_url: String,
        token_url: String,
        client_id: Option<String>,
        client_secret: Option<String>,
        device_authorization_endpoint: Option<String>,
        redirect_uri: Option<String>,
        scopes: Vec<String>,
    },
}

impl ResolvedProvider {
    /// Accessor for the shared scope list.
    pub fn scopes(&self) -> &[String] {
        match self {
            Self::Builtin { scopes, .. } | Self::Custom { scopes, .. } => scopes,
        }
    }
}

/// Resolve a server's `oauth:` block. Built-in providers come from
/// `builtin()`; unknown providers fall through to the §6.3 custom path,
/// which requires `authorize_url` + `token_url` on the config.
///
/// `OAuthConfig::scopes`, when non-empty, replaces the built-in defaults
/// entirely — the caller never needs to merge.
pub fn resolve(cfg: &OAuthConfig) -> Result<ResolvedProvider> {
    let provider = cfg
        .provider
        .as_deref()
        .ok_or_else(|| anyhow!("oauth.provider is required"))?;
    match builtin(provider) {
        Some(spec) => Ok(resolve_builtin(spec, cfg)),
        None => resolve_custom(provider, cfg),
    }
}

fn resolve_builtin(spec: ProviderSpec, cfg: &OAuthConfig) -> ResolvedProvider {
    let scopes = if cfg.scopes.is_empty() {
        spec.default_scopes.iter().map(|s| s.to_string()).collect()
    } else {
        cfg.scopes.clone()
    };
    ResolvedProvider::Builtin {
        provider_name: spec.name,
        authorize_url: spec.authorize_url,
        token_url: spec.token_url,
        callback: spec.callback,
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
    Ok(ResolvedProvider::Custom {
        provider_name: provider.to_string(),
        authorize_url,
        token_url,
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        device_authorization_endpoint: cfg.device_authorization_endpoint.clone(),
        redirect_uri: cfg.redirect_uri.clone(),
        scopes: cfg.scopes.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both env-touching tests below race the same OS env var — `set_var`
    // is unsound under concurrent reads, so serialize them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn builtin_client_id_requires_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized via ENV_LOCK; isolated env key.
        unsafe {
            std::env::remove_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID");
        }
        let err = builtin_client_id("anthropic-mcp").unwrap_err().to_string();
        assert!(err.contains("OPENAB_MCP_ANTHROPIC_CLIENT_ID"), "got: {err}");
    }

    #[test]
    fn builtin_client_id_uses_env_var_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized via ENV_LOCK; isolated env key.
        unsafe {
            std::env::set_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID", "anth-test-id");
        }
        let id = builtin_client_id("anthropic-mcp").unwrap();
        assert_eq!(id, "anth-test-id");
        unsafe {
            std::env::remove_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID");
        }
    }

    #[test]
    fn builtin_client_id_rejects_unknown_provider() {
        let err = builtin_client_id("does-not-exist").unwrap_err().to_string();
        assert!(err.contains("does-not-exist"), "got: {err}");
    }

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
        let ResolvedProvider::Builtin {
            provider_name,
            callback,
            scopes,
            ..
        } = resolve(&cfg).unwrap()
        else {
            panic!("expected Builtin variant");
        };
        assert_eq!(provider_name, "anthropic-mcp");
        assert_eq!(callback, ANTHROPIC_MCP.callback);
        assert_eq!(scopes.len(), ANTHROPIC_MCP.default_scopes.len());
    }

    #[test]
    fn resolve_builtin_uses_config_scopes_when_provided() {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            scopes: vec!["user:profile".to_string(), "user:inference".to_string()],
            ..Default::default()
        };
        let r = resolve(&cfg).unwrap();
        assert_eq!(r.scopes(), &["user:profile", "user:inference"]);
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
        let ResolvedProvider::Custom {
            provider_name,
            authorize_url,
            token_url,
            client_id,
            device_authorization_endpoint,
            scopes,
            ..
        } = resolve(&cfg).unwrap()
        else {
            panic!("expected Custom variant");
        };
        assert_eq!(provider_name, "linear");
        assert_eq!(authorize_url, "https://linear.app/oauth/authorize");
        assert_eq!(token_url, "https://api.linear.app/oauth/token");
        assert_eq!(client_id.as_deref(), Some("client-abc"));
        assert_eq!(
            device_authorization_endpoint.as_deref(),
            Some("https://linear.app/oauth/device"),
        );
        assert_eq!(scopes, vec!["read", "write"]);
    }

    #[test]
    fn resolve_custom_propagates_client_secret() {
        let cfg = OAuthConfig {
            provider: Some("acme".to_string()),
            authorize_url: Some("https://acme.example/authorize".to_string()),
            token_url: Some("https://acme.example/token".to_string()),
            client_id: Some("cid".to_string()),
            client_secret: Some("shhh".to_string()),
            ..Default::default()
        };
        let ResolvedProvider::Custom { client_secret, .. } = resolve(&cfg).unwrap() else {
            panic!("expected Custom variant");
        };
        assert_eq!(client_secret.as_deref(), Some("shhh"));
    }

    #[test]
    fn resolve_custom_minimal_two_urls_only() {
        let cfg = OAuthConfig {
            provider: Some("acme".to_string()),
            authorize_url: Some("https://acme.example/authorize".to_string()),
            token_url: Some("https://acme.example/token".to_string()),
            ..Default::default()
        };
        let ResolvedProvider::Custom {
            client_id,
            device_authorization_endpoint,
            scopes,
            ..
        } = resolve(&cfg).unwrap()
        else {
            panic!("expected Custom variant");
        };
        assert!(client_id.is_none());
        assert!(device_authorization_endpoint.is_none());
        assert!(scopes.is_empty());
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

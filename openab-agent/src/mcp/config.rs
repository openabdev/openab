//! `mcpServers` config schema + loader. See ADR §5.6.
//!
//! Loaded from `.openab/agent/mcp.json` (project) and `~/.openab/agent/mcp.json`
//! (global), project entries take precedence on name collision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub servers: HashMap<String, ServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default, rename = "tool_filter")]
        tool_filter: Option<ToolFilter>,
        #[serde(default = "default_request_timeout_secs")]
        request_timeout_secs: u64,
        #[serde(default)]
        log_level: Option<String>,
        #[serde(default)]
        ping_interval_secs: Option<u64>,
        #[serde(default)]
        ping_timeout_secs: Option<u64>,
    },
    Http {
        url: String,
        #[serde(default)]
        oauth: Option<OAuthConfig>,
        #[serde(default, rename = "tool_filter")]
        tool_filter: Option<ToolFilter>,
        #[serde(default = "default_request_timeout_secs")]
        request_timeout_secs: u64,
        #[serde(default)]
        log_level: Option<String>,
        #[serde(default)]
        ping_interval_secs: Option<u64>,
        #[serde(default)]
        ping_timeout_secs: Option<u64>,
    },
}

/// Default per-request timeout for MCP `tools/call` and `tools/list`
/// (ADR §5.6). Bounds a hung server so the agent turn can't stall
/// indefinitely; rmcp auto-emits a `notifications/cancelled` on expiry.
fn default_request_timeout_secs() -> u64 {
    60
}

impl ServerConfig {
    /// Static label used by the `mcp` meta-tool's `list_servers` action.
    /// Returning `&'static str` lets `snapshot()` avoid cloning the
    /// (potentially large) `Stdio { args, env, ... }` payload just to
    /// read the transport variant.
    pub fn transport_label(&self) -> &'static str {
        match self {
            ServerConfig::Stdio { .. } => "stdio",
            ServerConfig::Http { .. } => "http",
        }
    }

    /// `true` when the server is HTTP with an `oauth` block — used by the
    /// system-prompt catalogue (PR #959 F1 discovery slice) to hint that
    /// the LLM should ask the user to run `mcp login <name>` before calling.
    pub fn requires_oauth(&self) -> bool {
        matches!(self, ServerConfig::Http { oauth: Some(_), .. })
    }

    /// Per-request timeout for `tools/call` / `tools/list` against this
    /// server (ADR §5.6). Bounds a single MCP request, not the connection.
    pub fn request_timeout(&self) -> std::time::Duration {
        let secs = match self {
            ServerConfig::Stdio {
                request_timeout_secs,
                ..
            }
            | ServerConfig::Http {
                request_timeout_secs,
                ..
            } => *request_timeout_secs,
        };
        std::time::Duration::from_secs(secs)
    }

    /// Connect-time MCP `logging/setLevel` value for this server, if the
    /// operator pinned one (MCP §16 / row 584). `None` leaves the server at
    /// its default verbosity.
    pub fn log_level(&self) -> Option<&str> {
        match self {
            ServerConfig::Stdio { log_level, .. } | ServerConfig::Http { log_level, .. } => {
                log_level.as_deref()
            }
        }
    }

    /// Opt-in per-server liveness ping (MCP §5 ping / rows 273-279). Returns
    /// `Some((interval, timeout))` only when the operator set
    /// `ping_interval_secs`; `None` disables pinging entirely. The timeout
    /// bounds a single `PingRequest` and defaults to 5s when unset.
    pub fn ping_config(&self) -> Option<(std::time::Duration, std::time::Duration)> {
        let (interval_secs, timeout_secs) = match self {
            ServerConfig::Stdio {
                ping_interval_secs,
                ping_timeout_secs,
                ..
            }
            | ServerConfig::Http {
                ping_interval_secs,
                ping_timeout_secs,
                ..
            } => (*ping_interval_secs, *ping_timeout_secs),
        };
        let interval = std::time::Duration::from_secs(interval_secs?);
        let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(5));
        Some((interval, timeout))
    }
}

/// Parse an MCP `logging/setLevel` level string (case-insensitive) into the
/// rmcp `LoggingLevel`. Accepts the eight spec levels plus `warn` as an alias
/// for `warning`. Returns `None` for unrecognised input (caller skips
/// `setLevel` rather than failing the connection).
pub fn parse_logging_level(s: &str) -> Option<rmcp::model::LoggingLevel> {
    use rmcp::model::LoggingLevel::*;
    Some(match s.to_ascii_lowercase().as_str() {
        "debug" => Debug,
        "info" => Info,
        "notice" => Notice,
        "warning" | "warn" => Warning,
        "error" => Error,
        "critical" => Critical,
        "alert" => Alert,
        "emergency" => Emergency,
        _ => return None,
    })
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// OAuth block.
///
/// `provider` selects a built-in spec from `oauth::builtin()`. Setting it
/// to an unknown name + supplying `authorize_url` / `token_url` defines a
/// custom OAuth 2.1 provider (ADR §6.3). `discovery: true` opts into
/// RFC 8414 dynamic discovery and requires a non-empty
/// `discovery_allowlist` of domains (§6.4 SSRF guard).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
    /// Required for the paste-back branch of §6.4 on custom providers.
    /// Must match what's pre-registered with the provider's OAuth app
    /// (built-ins pin their callback in `ProviderSpec`). Ignored by the
    /// device-code branch.
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub discovery: bool,
    #[serde(default)]
    pub discovery_allowlist: Vec<String>,
}

impl OAuthConfig {
    /// Boot-time validation (ADR §6.3 / §6.4). `discovery: true` without an
    /// explicit allowlist is rejected — RFC 8414 lookups in multi-tenant
    /// deployments would otherwise become an SSRF vector.
    pub fn validate(&self, server: &str) -> Result<()> {
        if self.discovery && self.discovery_allowlist.is_empty() {
            return Err(anyhow!(
                "mcp server {server:?}: oauth.discovery=true requires \
                 a non-empty oauth.discovery_allowlist (ADR §6.3)"
            ));
        }
        // Custom (non-built-in) providers supply their own endpoint URLs, so
        // we enforce transport security on them here (MCP 2025-11-25 auth;
        // rows 217/218): OAuth endpoints must be https://, and the redirect
        // may only relax to http:// for the loopback interface (RFC 8252
        // §7.3). Built-ins pin vetted URLs in `ProviderSpec`, so are exempt.
        let is_custom = self
            .provider
            .as_deref()
            .map(|p| super::oauth::builtin(p).is_none())
            .unwrap_or(true);
        if is_custom {
            for (label, url) in [
                ("oauth.authorize_url", self.authorize_url.as_deref()),
                ("oauth.token_url", self.token_url.as_deref()),
            ] {
                if let Some(url) = url {
                    if !url.starts_with("https://") {
                        return Err(anyhow!(
                            "mcp server {server:?}: {label} must use https:// for a \
                             custom oauth provider (got {url:?})"
                        ));
                    }
                }
            }
            if let Some(redirect) = self.redirect_uri.as_deref() {
                if !is_loopback_or_https_redirect(redirect) {
                    return Err(anyhow!(
                        "mcp server {server:?}: oauth.redirect_uri must use https:// \
                         or http://localhost (got {redirect:?})"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A custom-provider redirect URI is acceptable when it is `https://`, or
/// `http://` pointing at the loopback interface (RFC 8252 §7.3 native-app
/// redirect). Host is matched by prefix to keep the check dependency-free.
fn is_loopback_or_https_redirect(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    match url.strip_prefix("http://") {
        Some(rest) => {
            let host = rest.split(['/', ':']).next().unwrap_or(rest);
            host == "localhost" || host == "127.0.0.1" || rest.starts_with("[::1]")
        }
        None => false,
    }
}

impl McpConfig {
    /// Load + merge global and project configs from the standard locations.
    /// Missing files are treated as empty.
    pub fn load() -> Result<Self> {
        let global = home_dir().map(|h| h.join(".openab/agent/mcp.json"));
        let project = std::env::current_dir()
            .ok()
            .map(|c| c.join(".openab/agent/mcp.json"));
        Self::load_layered(global.as_deref(), project.as_deref())
    }

    /// Load + merge two layers; project wins on name collision.
    pub fn load_layered(global: Option<&Path>, project: Option<&Path>) -> Result<Self> {
        let mut merged = Self::default();
        for path in [global, project].into_iter().flatten() {
            if !path.exists() {
                continue;
            }
            let layer = Self::load_file(path)?;
            merged.servers.extend(layer.servers);
        }
        merged.validate()?;
        Ok(merged)
    }

    /// Validate every server's `oauth` block (ADR §6.3 boot check). Returns
    /// the first failure — finer-grained per-server isolation lives in §5.6.
    pub fn validate(&self) -> Result<()> {
        for (name, server) in &self.servers {
            if let ServerConfig::Http {
                oauth: Some(oauth), ..
            } = server
            {
                oauth.validate(name)?;
            }
        }
        Ok(())
    }

    fn load_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read mcp config {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parse mcp config {}", path.display()))
    }
}

impl ServerConfig {
    /// Return a copy with `${env:VAR}` placeholders resolved against the
    /// process environment. Missing env vars are an error for that server;
    /// callers should skip the server and continue (ADR §5.6 "per-server
    /// failure isolated"). `name` is the server name used in error context.
    pub fn resolved(&self, name: &str) -> Result<Self> {
        self.resolved_with_env(name, &std::env::vars().collect())
    }

    fn resolved_with_env(&self, name: &str, env: &HashMap<String, String>) -> Result<Self> {
        let json = serde_json::to_value(self)?;
        let resolved = interpolate_value(json, env)
            .with_context(|| format!("resolve env for mcp server {name:?}"))?;
        Ok(serde_json::from_value(resolved)?)
    }
}

fn interpolate_value(
    value: serde_json::Value,
    env: &HashMap<String, String>,
) -> Result<serde_json::Value> {
    use serde_json::Value;
    match value {
        Value::String(s) => Ok(Value::String(interpolate_env(&s, env)?)),
        Value::Array(items) => items
            .into_iter()
            .map(|v| interpolate_value(v, env))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(map) => map
            .into_iter()
            .map(|(k, v)| interpolate_value(v, env).map(|v| (k, v)))
            .collect::<Result<serde_json::Map<_, _>>>()
            .map(Value::Object),
        other => Ok(other),
    }
}

/// Replace `${env:VAR}` tokens in `input` with the matching env value.
/// Missing variables produce an error naming the offender.
pub fn interpolate_env(input: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "${env:".len()..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("unterminated ${{env:..}} in {input:?}"))?;
        let var = &after[..end];
        let val = env
            .get(var)
            .ok_or_else(|| anyhow!("env var ${var} not set (referenced by mcp config)"))?;
        out.push_str(val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn interpolate_replaces_tokens() {
        let e = env(&[("FOO", "bar"), ("X", "y")]);
        assert_eq!(
            interpolate_env("a${env:FOO}b${env:X}", &e).unwrap(),
            "abarby"
        );
    }

    #[test]
    fn interpolate_passes_through_plain_strings() {
        let e = env(&[]);
        assert_eq!(interpolate_env("plain", &e).unwrap(), "plain");
    }

    #[test]
    fn interpolate_errors_on_missing_var() {
        let e = env(&[]);
        let err = interpolate_env("${env:MISSING}", &e)
            .unwrap_err()
            .to_string();
        assert!(err.contains("MISSING"), "expected MISSING in error: {err}");
    }

    #[test]
    fn interpolate_errors_on_unterminated() {
        let e = env(&[("FOO", "bar")]);
        assert!(interpolate_env("${env:FOO", &e).is_err());
    }

    #[test]
    fn parses_stdio_and_http_servers() {
        let json = r#"{
            "mcpServers": {
                "fs": {
                    "type": "stdio",
                    "command": "mcp-server-filesystem",
                    "args": ["/workspace"],
                    "tool_filter": { "include": ["read_*"] }
                },
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": { "provider": "linear" }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        match cfg.servers.get("fs").unwrap() {
            ServerConfig::Stdio {
                command,
                args,
                tool_filter,
                ..
            } => {
                assert_eq!(command, "mcp-server-filesystem");
                assert_eq!(args, &vec!["/workspace".to_string()]);
                assert_eq!(tool_filter.as_ref().unwrap().include, vec!["read_*"]);
            }
            _ => panic!("expected stdio"),
        }
        match cfg.servers.get("linear").unwrap() {
            ServerConfig::Http { url, oauth, .. } => {
                assert_eq!(url, "https://mcp.linear.app/mcp");
                assert_eq!(oauth.as_ref().unwrap().provider.as_deref(), Some("linear"));
            }
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn resolved_substitutes_env_in_args() {
        let env = env(&[("MCP_TEST_TOKEN", "secret123")]);
        let cfg = ServerConfig::Stdio {
            command: "github-mcp-server".into(),
            args: vec!["--token".into(), "${env:MCP_TEST_TOKEN}".into()],
            env: HashMap::new(),
            tool_filter: None,
            request_timeout_secs: default_request_timeout_secs(),
            log_level: None,
            ping_interval_secs: None,
            ping_timeout_secs: None,
        };
        match cfg.resolved_with_env("github", &env).unwrap() {
            ServerConfig::Stdio { args, .. } => {
                assert_eq!(args[1], "secret123");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn ping_config_opt_in_and_default_timeout() {
        // Absent ping_interval_secs => pinging disabled.
        let off: ServerConfig =
            serde_json::from_str(r#"{"type":"stdio","command":"x"}"#).unwrap();
        assert!(off.ping_config().is_none());

        // interval set, timeout omitted => 5s default timeout.
        let on: ServerConfig = serde_json::from_str(
            r#"{"type":"http","url":"https://e.com/mcp","ping_interval_secs":30}"#,
        )
        .unwrap();
        let (interval, timeout) = on.ping_config().unwrap();
        assert_eq!(interval, std::time::Duration::from_secs(30));
        assert_eq!(timeout, std::time::Duration::from_secs(5));

        // both set => both honoured.
        let both: ServerConfig = serde_json::from_str(
            r#"{"type":"stdio","command":"x","ping_interval_secs":15,"ping_timeout_secs":3}"#,
        )
        .unwrap();
        assert_eq!(
            both.ping_config().unwrap(),
            (
                std::time::Duration::from_secs(15),
                std::time::Duration::from_secs(3)
            )
        );
    }

    #[test]
    fn merge_project_wins() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.json");
        let project = dir.path().join("project.json");
        std::fs::write(
            &global,
            r#"{"mcpServers":{"fs":{"type":"stdio","command":"global-fs"},"x":{"type":"stdio","command":"global-x"}}}"#,
        )
        .unwrap();
        std::fs::write(
            &project,
            r#"{"mcpServers":{"fs":{"type":"stdio","command":"project-fs"}}}"#,
        )
        .unwrap();
        let cfg = McpConfig::load_layered(Some(&global), Some(&project)).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        match cfg.servers.get("fs").unwrap() {
            ServerConfig::Stdio { command, .. } => assert_eq!(command, "project-fs"),
            _ => unreachable!(),
        }
        match cfg.servers.get("x").unwrap() {
            ServerConfig::Stdio { command, .. } => assert_eq!(command, "global-x"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parses_custom_oauth_provider_fields() {
        let json = r#"{
            "mcpServers": {
                "custom": {
                    "type": "http",
                    "url": "https://example.com/mcp",
                    "oauth": {
                        "provider": "custom",
                        "authorize_url": "https://example.com/oauth/authorize",
                        "token_url": "https://example.com/oauth/token",
                        "client_id": "abc123",
                        "device_authorization_endpoint": "https://example.com/oauth/device",
                        "discovery": true,
                        "discovery_allowlist": ["*.example.com"]
                    }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let ServerConfig::Http {
            oauth: Some(oauth), ..
        } = cfg.servers.get("custom").unwrap()
        else {
            panic!("expected http with oauth");
        };
        assert_eq!(
            oauth.authorize_url.as_deref(),
            Some("https://example.com/oauth/authorize"),
        );
        assert_eq!(
            oauth.token_url.as_deref(),
            Some("https://example.com/oauth/token"),
        );
        assert_eq!(oauth.client_id.as_deref(), Some("abc123"));
        assert_eq!(
            oauth.device_authorization_endpoint.as_deref(),
            Some("https://example.com/oauth/device"),
        );
        assert!(oauth.discovery);
        assert_eq!(oauth.discovery_allowlist, vec!["*.example.com".to_string()]);
    }

    #[test]
    fn validate_rejects_discovery_without_allowlist() {
        let oauth = OAuthConfig {
            provider: Some("custom".into()),
            discovery: true,
            ..Default::default()
        };
        let err = oauth.validate("srv").unwrap_err().to_string();
        assert!(err.contains("discovery_allowlist"), "got: {err}");
        assert!(err.contains("srv"), "got: {err}");
    }

    #[test]
    fn validate_accepts_discovery_with_allowlist() {
        let oauth = OAuthConfig {
            provider: Some("custom".into()),
            discovery: true,
            discovery_allowlist: vec!["*.example.com".into()],
            ..Default::default()
        };
        oauth.validate("srv").unwrap();
    }

    #[test]
    fn load_layered_rejects_invalid_discovery_config() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.json");
        std::fs::write(
            &project,
            r#"{"mcpServers":{"bad":{"type":"http","url":"https://example.com","oauth":{"provider":"custom","discovery":true}}}}"#,
        )
        .unwrap();
        let err = McpConfig::load_layered(None, Some(&project))
            .unwrap_err()
            .to_string();
        assert!(err.contains("discovery_allowlist"), "got: {err}");
    }

    fn custom(authorize: &str, token: &str) -> OAuthConfig {
        OAuthConfig {
            provider: Some("custom".into()),
            authorize_url: Some(authorize.into()),
            token_url: Some(token.into()),
            ..Default::default()
        }
    }

    #[test]
    fn validate_rejects_http_authorize_url() {
        let oauth = custom("http://issuer.example/authorize", "https://issuer.example/token");
        let err = oauth.validate("srv").unwrap_err().to_string();
        assert!(err.contains("oauth.authorize_url"), "got: {err}");
        assert!(err.contains("https://"), "got: {err}");
    }

    #[test]
    fn validate_rejects_http_token_url() {
        let oauth = custom("https://issuer.example/authorize", "http://issuer.example/token");
        let err = oauth.validate("srv").unwrap_err().to_string();
        assert!(err.contains("oauth.token_url"), "got: {err}");
    }

    #[test]
    fn validate_accepts_https_urls() {
        let oauth = custom(
            "https://issuer.example/authorize",
            "https://issuer.example/token",
        );
        oauth.validate("srv").unwrap();
    }

    #[test]
    fn validate_rejects_non_localhost_http_redirect_uri() {
        let mut oauth = custom(
            "https://issuer.example/authorize",
            "https://issuer.example/token",
        );
        oauth.redirect_uri = Some("http://app.example/callback".into());
        let err = oauth.validate("srv").unwrap_err().to_string();
        assert!(err.contains("oauth.redirect_uri"), "got: {err}");
    }

    #[test]
    fn validate_accepts_localhost_http_redirect_uri() {
        let mut oauth = custom(
            "https://issuer.example/authorize",
            "https://issuer.example/token",
        );
        oauth.redirect_uri = Some("http://localhost:8765/callback".into());
        oauth.validate("srv").unwrap();
    }
}

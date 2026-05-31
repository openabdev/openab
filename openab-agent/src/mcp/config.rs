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
    },
    Http {
        url: String,
        #[serde(default)]
        oauth: Option<OAuthConfig>,
        #[serde(default, rename = "tool_filter")]
        tool_filter: Option<ToolFilter>,
    },
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
        Ok(())
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
        let json = serde_json::to_value(self)?;
        let resolved = interpolate_value(json, &std::env::vars().collect())
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
        // SAFETY: single-threaded test; isolated env key.
        unsafe {
            std::env::set_var("MCP_TEST_TOKEN", "secret123");
        }
        let cfg = ServerConfig::Stdio {
            command: "github-mcp-server".into(),
            args: vec!["--token".into(), "${env:MCP_TEST_TOKEN}".into()],
            env: HashMap::new(),
            tool_filter: None,
        };
        match cfg.resolved("github").unwrap() {
            ServerConfig::Stdio { args, .. } => {
                assert_eq!(args[1], "secret123");
            }
            _ => unreachable!(),
        }
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
}

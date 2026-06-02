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

    /// `true` when the server is HTTP with an `oauth` block — used by the
    /// system-prompt catalogue (PR #959 F1 discovery slice) to hint that
    /// the LLM should ask the user to run `mcp login <name>` before calling.
    pub fn requires_oauth(&self) -> bool {
        matches!(self, ServerConfig::Http { oauth: Some(_), .. })
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// OAuth block. Phase 1 only parses `provider` + `scopes`; custom-provider
/// fields (§6.3: `authorize_url`, `token_url`, `device_authorization_endpoint`,
/// `discovery`, `discovery_allowlist`) land with the Phase 2 auth slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
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
        Ok(merged)
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
    fn resolved_errors_on_missing_env_var_with_var_name() {
        // chaodu F9 (#959 review): contract is that a missing env var
        // referenced via `${env:VAR}` in any config field surfaces an error
        // naming the offender, so users can fix `mcp.json` instead of
        // chasing a generic parse failure.
        let cfg = ServerConfig::Stdio {
            command: "github-mcp-server".into(),
            args: vec!["--token".into(), "${env:CHAODU_F9_MISSING}".into()],
            env: HashMap::new(),
            tool_filter: None,
        };
        let err = format!(
            "{:#}",
            cfg.resolved_with_env("github", &env(&[])).unwrap_err()
        );
        assert!(
            err.contains("CHAODU_F9_MISSING"),
            "expected missing var name in error: {err}"
        );
        assert!(
            err.contains("github"),
            "expected server name in error context: {err}"
        );
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
        };
        match cfg.resolved_with_env("github", &env).unwrap() {
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
}

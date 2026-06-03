//! Single `mcp` meta-tool the LLM sees. See ADR §5.2 + §5.3.
//!
//! Dispatches discovery / call actions plus the OAuth login actions from
//! ADR §6.4.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::runtime::{McpRuntimeManager, ServerStatus};

/// Deserialized form of the meta-tool's input JSON (ADR §5.2). The LLM
/// sends `{ "action": "...", ... }`; `tag = "action"` routes by that field.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Help,
    ListServers,
    ListTools {
        server: String,
    },
    DescribeTool {
        server: String,
        tool: String,
    },
    Call {
        server: String,
        tool: String,
        #[serde(default)]
        arguments: Value,
    },
    Status {
        #[serde(default)]
        server: Option<String>,
    },
    Login {
        server: String,
        #[serde(default)]
        flow: Option<String>,
    },
    CompleteLogin {
        server: String,
        redirect_url: String,
    },
}

/// Entry point — the LLM tool dispatcher hands us a deserialized `Action`
/// and we return the JSON payload that becomes the tool result.
pub async fn dispatch(manager: &McpRuntimeManager, action: Action) -> Result<Value> {
    match action {
        Action::Help => Ok(json!(HELP)),
        Action::ListServers => Ok(list_servers(manager).await),
        Action::ListTools { server } => list_tools(manager, &server).await,
        Action::DescribeTool { server, tool } => describe_tool(manager, &server, &tool).await,
        Action::Call {
            server,
            tool,
            arguments,
        } => call_tool(manager, &server, &tool, arguments).await,
        Action::Status { server } => Ok(status(manager, server.as_deref()).await),
        Action::Login { server, flow } => login(manager, &server, flow.as_deref()).await,
        Action::CompleteLogin {
            server,
            redirect_url,
        } => complete_login(manager, &server, &redirect_url).await,
    }
}

const HELP: &str = "\
The `mcp` tool lets you talk to configured MCP servers.

Actions:
  help                         show this message
  list_servers                 list configured servers and status
  list_tools(server)           list tools exposed by a server
  describe_tool(server, tool)  show input_schema for one tool
  call(server, tool, args)     invoke a tool
  status(server?)              per-server health + last error
  login(server, flow?)         start OAuth login (auto/paste/device)
  complete_login(server, redirect_url)
                               finish paste-back OAuth after user pastes URL

Connections are lazy: the first action that needs a server spawns its \
child process and runs the handshake. Idle servers are evicted after \
the configured TTL.";

async fn login(manager: &McpRuntimeManager, server: &str, flow: Option<&str>) -> Result<Value> {
    match flow {
        Some("device") => login_device(manager, server).await,
        Some("paste") => login_paste(manager, server).await,
        Some(other) => Err(anyhow!(
            "unsupported mcp login flow {other:?}; expected \"paste\", \"device\", or omit flow for auto"
        )),
        None => match manager.preferred_login_flow(server).await? {
            "device" => login_device(manager, server).await,
            _ => login_paste(manager, server).await,
        },
    }
}

async fn login_paste(manager: &McpRuntimeManager, server: &str) -> Result<Value> {
    let start = manager.start_paste_login(server).await?;
    Ok(json!({
        "flow": "paste",
        "server": server,
        "authorize_url": start.authorize_url,
        "state": start.state,
    }))
}

async fn login_device(manager: &McpRuntimeManager, server: &str) -> Result<Value> {
    let start = manager.start_device_login(server).await?;
    Ok(json!({
        "flow": "device",
        "server": server,
        "user_code": start.user_code,
        "verification_uri": start.verification_uri,
        "verification_uri_complete": start.verification_uri_complete,
        "expires_in": start.expires_in,
    }))
}

async fn complete_login(
    manager: &McpRuntimeManager,
    server: &str,
    redirect_url: &str,
) -> Result<Value> {
    manager.complete_login(server, redirect_url).await?;
    Ok(json!({
        "server": server,
        "status": "completed",
    }))
}

async fn call_tool(
    manager: &McpRuntimeManager,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    // Lenient arg coercion: LLMs often send `null` or omit `arguments`
    // for no-arg tools; rejecting those would make zero-arg calls
    // fragile. Only real type errors (string, number, array, bool)
    // are refused.
    let args_map = match arguments {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            return Err(anyhow!(
                "mcp call arguments must be a JSON object (or null/omitted for no-arg tools), got {other}"
            ));
        }
    };
    manager
        .connect(server)
        .await
        .with_context(|| format!("connect mcp server {server:?}"))?;
    let peer = manager.arc_peer(server).await?;
    let params = rmcp::model::CallToolRequestParams::new(tool.to_string()).with_arguments(args_map);
    let result = peer
        .call_tool(params)
        .await
        .with_context(|| format!("call_tool {tool:?} on {server:?}"))?;
    serde_json::to_value(&result).context("serialize CallToolResult")
}

/// Lazy-connect + list all tools on `server`. Shared by `list_tools` /
/// `describe_tool` (and the planned `tools_cache` on ServerHandle will plug
/// in here). The `Arc<RunningService>` clone lets the I/O `.await` run with
/// no runtime lock held.
async fn fetch_tools(manager: &McpRuntimeManager, server: &str) -> Result<Vec<rmcp::model::Tool>> {
    manager
        .connect(server)
        .await
        .with_context(|| format!("connect mcp server {server:?}"))?;
    let peer = manager.arc_peer(server).await?;
    peer.list_all_tools()
        .await
        .with_context(|| format!("list_all_tools on {server:?}"))
}

async fn list_tools(manager: &McpRuntimeManager, server: &str) -> Result<Value> {
    let entries: Vec<Value> = fetch_tools(manager, server)
        .await?
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
            })
        })
        .collect();
    Ok(Value::Array(entries))
}

async fn describe_tool(manager: &McpRuntimeManager, server: &str, tool: &str) -> Result<Value> {
    // Progressive disclosure (ADR §5.2): `list_tools` returns compact
    // `{name, description}`; this action returns the full `input_schema`
    // for one tool. MCP has no single-tool query, so we list + filter.
    let tool_def = fetch_tools(manager, server)
        .await?
        .into_iter()
        .find(|t| t.name.as_ref() == tool)
        .ok_or_else(|| anyhow!("no tool {tool:?} on mcp server {server:?}"))?;
    Ok(json!({
        "name": tool_def.name,
        "description": tool_def.description,
        "input_schema": tool_def.input_schema,
    }))
}

async fn status(manager: &McpRuntimeManager, filter: Option<&str>) -> Value {
    let snapshot = manager.snapshot().await;
    let entries: Vec<Value> = snapshot
        .into_iter()
        .filter(|(name, _, _)| match filter {
            Some(f) => f == name.as_str(),
            None => true,
        })
        .map(|(name, status, transport)| {
            let last_error = match &status {
                ServerStatus::Failed(msg) => Some(msg.clone()),
                _ => None,
            };
            json!({
                "name": name,
                "status": status_label(&status),
                "transport": transport,
                "last_error": last_error,
            })
        })
        .collect();
    Value::Array(entries)
}

async fn list_servers(manager: &McpRuntimeManager) -> Value {
    let snapshot = manager.snapshot().await;
    let entries: Vec<Value> = snapshot
        .into_iter()
        .map(|(name, status, transport)| {
            json!({
                "name": name,
                "status": status_label(&status),
                "transport": transport,
            })
        })
        .collect();
    Value::Array(entries)
}

fn status_label(status: &ServerStatus) -> &'static str {
    match status {
        ServerStatus::Disconnected => "disconnected",
        ServerStatus::Connecting => "connecting",
        ServerStatus::Connected => "connected",
        ServerStatus::NeedsAuth => "needs_auth",
        ServerStatus::Failed(_) => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::McpConfig;

    fn mgr_from(json: &str) -> McpRuntimeManager {
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        McpRuntimeManager::from_config(cfg)
    }

    fn mgr_from_with_temp_auth(json: &str) -> (McpRuntimeManager, tempfile::TempDir) {
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mgr = McpRuntimeManager::from_config_with_auth_path(cfg, dir.path().join("auth.json"));
        (mgr, dir)
    }

    fn linear_custom_cfg() -> &'static str {
        r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "client_id": "linear-client",
                        "redirect_uri": "https://example.com/callback",
                        "scopes": ["read"]
                    }
                }
            }
        }"#
    }

    #[tokio::test]
    async fn help_returns_doc_string() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let result = dispatch(&mgr, Action::Help).await.unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains("list_servers"));
        assert!(s.contains("call(server, tool"));
        assert!(s.contains("complete_login"));
    }

    #[tokio::test]
    async fn list_servers_reports_name_status_transport() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                    "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
                }
            }"#,
        );
        let result = dispatch(&mgr, Action::ListServers).await.unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let by_name: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|e| (e["name"].as_str().unwrap(), e))
            .collect();
        assert_eq!(by_name["fs"]["transport"], "stdio");
        assert_eq!(by_name["fs"]["status"], "disconnected");
        assert_eq!(by_name["linear"]["transport"], "http");
    }

    #[tokio::test]
    async fn list_servers_empty_yields_empty_array() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let result = dispatch(&mgr, Action::ListServers).await.unwrap();
        assert!(result.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn call_rejects_non_object_arguments() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "true" }
                }
            }"#,
        );
        let err = dispatch(
            &mgr,
            Action::Call {
                server: "fs".into(),
                tool: "read".into(),
                arguments: json!("oops, a string"),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    #[tokio::test]
    async fn call_null_arguments_passes_validation_and_reaches_connect() {
        // Null args should be coerced to {} and fail at the *connect* step
        // (binary doesn't exist), not at the validation step.
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "broken": {
                        "type": "stdio",
                        "command": "/nonexistent/openab-mcp-test-stub-zzz"
                    }
                }
            }"#,
        );
        let err = dispatch(
            &mgr,
            Action::Call {
                server: "broken".into(),
                tool: "read".into(),
                arguments: Value::Null,
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("connect mcp server"), "got: {err}");
        assert!(!err.contains("must be a JSON object"), "got: {err}");
    }

    #[tokio::test]
    async fn list_tools_propagates_connect_failure() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "broken": {
                        "type": "stdio",
                        "command": "/nonexistent/path/openab-mcp-test-stub-zzz"
                    }
                }
            }"#,
        );
        let err = dispatch(
            &mgr,
            Action::ListTools {
                server: "broken".into(),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("connect mcp server"), "got: {err}");
    }

    #[tokio::test]
    async fn describe_tool_propagates_connect_failure() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "broken": {
                        "type": "stdio",
                        "command": "/nonexistent/path/openab-mcp-test-stub-zzz"
                    }
                }
            }"#,
        );
        let err = dispatch(
            &mgr,
            Action::DescribeTool {
                server: "broken".into(),
                tool: "read".into(),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("connect mcp server"), "got: {err}");
    }

    #[tokio::test]
    async fn status_lists_each_server_with_null_last_error_by_default() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                    "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
                }
            }"#,
        );
        let result = dispatch(&mgr, Action::Status { server: None })
            .await
            .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for e in entries {
            assert_eq!(e["status"], "disconnected");
            assert!(e["last_error"].is_null());
        }
    }

    #[tokio::test]
    async fn status_filter_by_server_returns_single_entry() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                    "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
                }
            }"#,
        );
        let result = dispatch(
            &mgr,
            Action::Status {
                server: Some("fs".into()),
            },
        )
        .await
        .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "fs");
        assert_eq!(entries[0]["transport"], "stdio");
    }

    #[tokio::test]
    async fn status_unknown_filter_returns_empty_array() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" }
                }
            }"#,
        );
        let result = dispatch(
            &mgr,
            Action::Status {
                server: Some("nope".into()),
            },
        )
        .await
        .unwrap();
        assert!(result.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_surfaces_last_error_after_failed_connect() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "broken": {
                        "type": "stdio",
                        "command": "/nonexistent/path/openab-mcp-test-stub-zzz"
                    }
                }
            }"#,
        );
        let _ = dispatch(
            &mgr,
            Action::ListTools {
                server: "broken".into(),
            },
        )
        .await;
        let result = dispatch(&mgr, Action::Status { server: None })
            .await
            .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["status"], "failed");
        let last_error = entries[0]["last_error"].as_str().unwrap();
        assert!(last_error.contains("spawn"), "got: {last_error}");
    }

    #[tokio::test]
    async fn login_auto_starts_paste_flow_for_custom_provider() {
        let (mgr, _dir) = mgr_from_with_temp_auth(linear_custom_cfg());
        let result = dispatch(
            &mgr,
            Action::Login {
                server: "linear".into(),
                flow: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(result["flow"], "paste");
        assert_eq!(result["server"], "linear");
        assert!(result["authorize_url"]
            .as_str()
            .unwrap()
            .starts_with("https://linear.app/oauth/authorize?"));
        assert!(result["state"].as_str().unwrap().len() > 10);
    }

    #[tokio::test]
    async fn login_rejects_unknown_flow() {
        let (mgr, _dir) = mgr_from_with_temp_auth(linear_custom_cfg());
        let err = dispatch(
            &mgr,
            Action::Login {
                server: "linear".into(),
                flow: Some("browser".into()),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("unsupported"), "got: {err}");
    }

    #[tokio::test]
    async fn complete_login_dispatches_to_runtime() {
        let (mgr, _dir) = mgr_from_with_temp_auth(linear_custom_cfg());
        let err = dispatch(
            &mgr,
            Action::CompleteLogin {
                server: "linear".into(),
                redirect_url: "https://example.com/callback?code=c&state=s".into(),
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("no pending login"), "got: {err}");
    }

    #[test]
    fn action_deserializes_from_meta_tool_payload() {
        let payload = json!({
            "action": "call",
            "server": "github",
            "tool": "create_issue",
            "arguments": { "title": "x" }
        });
        let action: Action = serde_json::from_value(payload).unwrap();
        match action {
            Action::Call {
                server,
                tool,
                arguments,
            } => {
                assert_eq!(server, "github");
                assert_eq!(tool, "create_issue");
                assert_eq!(arguments["title"], "x");
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn action_deserializes_login_and_complete_login() {
        let action: Action =
            serde_json::from_value(json!({ "action": "login", "server": "linear" })).unwrap();
        assert!(matches!(
            action,
            Action::Login {
                flow: None,
                server
            } if server == "linear"
        ));

        let action: Action = serde_json::from_value(json!({
            "action": "complete_login",
            "server": "linear",
            "redirect_url": "https://example.com/cb?code=c&state=s"
        }))
        .unwrap();
        assert!(matches!(action, Action::CompleteLogin { .. }));
    }

    #[test]
    fn action_status_server_is_optional() {
        let action: Action = serde_json::from_value(json!({ "action": "status" })).unwrap();
        assert!(matches!(action, Action::Status { server: None }));
        let action: Action =
            serde_json::from_value(json!({ "action": "status", "server": "fs" })).unwrap();
        assert!(matches!(action, Action::Status { server: Some(_) }));
    }
}

//! Single `mcp` meta-tool the LLM sees. See ADR §5.2 + §5.3.
//!
//! Phase 1 scope: action enum + dispatch wiring + the two no-IO actions
//! (`help`, `list_servers`). The IO-bearing actions (`list_tools`,
//! `describe_tool`, `call`, `status`) return a `not yet implemented`
//! error so the contract surface is visible to callers while the
//! `RunningService` borrow path lands in the next slice. The Phase 2
//! `login` / `complete_login` actions land with the OAuth slice.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::runtime::{McpRuntimeManager, ServerStatus};

/// Deserialized form of the meta-tool's input JSON (ADR §5.2). The LLM
/// sends `{ "action": "...", ... }`; `tag = "action"` routes by that field.
#[allow(dead_code)] // wired into agent.rs execute_tool dispatch in the next slice
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
}

/// Entry point — the LLM tool dispatcher hands us a deserialized `Action`
/// and we return the JSON payload that becomes the tool result.
#[allow(dead_code)] // wired into agent.rs execute_tool dispatch in the next slice
pub async fn dispatch(manager: &McpRuntimeManager, action: Action) -> Result<Value> {
    match action {
        Action::Help => Ok(json!(HELP)),
        Action::ListServers => Ok(list_servers(manager).await),
        Action::ListTools { server } => list_tools(manager, &server).await,
        Action::Call {
            server,
            tool,
            arguments,
        } => call_tool(manager, &server, &tool, arguments).await,
        other => Err(anyhow!("{}", not_implemented_msg(&other))),
    }
}

/// Error body for actions whose handler hasn't landed yet. Mentions the
/// requested action and the supported set so the LLM can recover by
/// falling back to the native `read` / `write` / `edit` / `bash` tools
/// instead of retrying the same action blindly.
fn not_implemented_msg(action: &Action) -> String {
    let name = match action {
        Action::Help => "help",
        Action::ListServers => "list_servers",
        Action::ListTools { .. } => "list_tools",
        Action::DescribeTool { .. } => "describe_tool",
        Action::Call { .. } => "call",
        Action::Status { .. } => "status",
    };
    format!(
        "mcp action '{name}' is not yet implemented (phase 1 scaffold). \
         Currently supported: 'help', 'list_servers', 'list_tools', 'call'. \
         To complete your task right now, fall back to the native agent tools \
         (read, write, edit, bash)."
    )
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

Connections are lazy: the first action that needs a server spawns its \
child process and runs the handshake. Idle servers are evicted after \
the configured TTL.";

async fn call_tool(
    manager: &McpRuntimeManager,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value> {
    // Lenient arg coercion per Mira's Tick 18 review: LLMs often send
    // `null` or omit `arguments` for no-arg tools; rejecting those would
    // make zero-arg calls fragile. Only real type errors (string, number,
    // array, bool) are refused.
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

async fn list_tools(manager: &McpRuntimeManager, server: &str) -> Result<Value> {
    // Lazy connect per ADR §5.3 — idempotent if already Connected.
    manager
        .connect(server)
        .await
        .with_context(|| format!("connect mcp server {server:?}"))?;
    let peer = manager.arc_peer(server).await?;
    // Arc lets the I/O `.await` run with no runtime lock held.
    let tools = peer
        .list_all_tools()
        .await
        .with_context(|| format!("list_all_tools on {server:?}"))?;
    let entries: Vec<Value> = tools
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

    #[tokio::test]
    async fn help_returns_doc_string() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let result = dispatch(&mgr, Action::Help).await.unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains("list_servers"));
        assert!(s.contains("call(server, tool"));
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
    async fn unimplemented_actions_name_themselves_and_guide_fallback() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let cases = [
            (
                Action::DescribeTool {
                    server: "fs".into(),
                    tool: "read".into(),
                },
                "describe_tool",
            ),
            (Action::Status { server: None }, "status"),
        ];
        for (action, expected_name) in cases {
            let err = dispatch(&mgr, action).await.unwrap_err().to_string();
            assert!(err.contains(expected_name), "missing action name: {err}");
            assert!(err.contains("not yet implemented"), "got: {err}");
            assert!(
                err.contains("read, write, edit, bash"),
                "missing fallback: {err}"
            );
        }
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
    fn action_status_server_is_optional() {
        let action: Action = serde_json::from_value(json!({ "action": "status" })).unwrap();
        assert!(matches!(action, Action::Status { server: None }));
        let action: Action =
            serde_json::from_value(json!({ "action": "status", "server": "fs" })).unwrap();
        assert!(matches!(action, Action::Status { server: Some(_) }));
    }
}

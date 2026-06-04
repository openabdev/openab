//! Single `mcp` meta-tool the LLM sees. See ADR §5.2 + §5.3.
//!
//! Phase 1 scope: action enum + dispatch wiring + all six Phase 1 actions
//! (`help`, `list_servers`, `list_tools`, `describe_tool`, `call`, `status`).
//! The Phase 2 `login` / `complete_login` actions land with the OAuth slice.

use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use rmcp::model::{
    CallToolRequest, ClientRequest, ListToolsRequest, PaginatedRequestParams, ServerResult,
    TaskSupport,
};
use rmcp::service::{PeerRequestOptions, RoleClient, RunningService, ServiceError};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::runtime::{McpRuntimeManager, OpenabClientHandler, ServerStatus};

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
}

/// Entry point — the LLM tool dispatcher hands us a deserialized `Action`
/// and we return the JSON payload that becomes the tool result.
pub async fn dispatch(manager: &McpRuntimeManager, action: Action) -> Result<(Value, Option<bool>)> {
    match action {
        Action::Help => Ok((json!(HELP), None)),
        Action::ListServers => Ok((list_servers(manager).await, None)),
        Action::ListTools { server } => list_tools(manager, &server).await.map(|v| (v, None)),
        Action::DescribeTool { server, tool } => {
            describe_tool(manager, &server, &tool).await.map(|v| (v, None))
        }
        Action::Call {
            server,
            tool,
            arguments,
        } => call_tool(manager, &server, &tool, arguments).await,
        Action::Status { server } => Ok((status(manager, server.as_deref()).await, None)),
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

Connections are lazy: the first action that needs a server spawns its \
child process and runs the handshake. Idle servers are evicted after \
the configured TTL.";

/// Fail fast if the server never advertised the `tools` capability in its
/// `InitializeResult`. Without this guard a `tools/list` or `tools/call`
/// against such a server surfaces as a generic JSON-RPC error; here we turn
/// it into a clear, server-named diagnostic (MCP capability gating, Row 65).
fn ensure_tools_capability(
    peer: &RunningService<RoleClient, OpenabClientHandler>,
    server: &str,
) -> Result<()> {
    let info = peer
        .peer_info()
        .ok_or_else(|| anyhow!("mcp server {server:?} returned no initialize result"))?;
    if info.capabilities.tools.is_none() {
        return Err(anyhow!(
            "mcp server {server:?} does not advertise tools capability"
        ));
    }
    Ok(())
}

async fn call_tool(
    manager: &McpRuntimeManager,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<(Value, Option<bool>)> {
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
    // Audit trail: hash the args actually sent on the wire (never the
    // plaintext — could carry secrets). sha2 is already a dep (auth.rs).
    let args_sha256 = Sha256::digest(serde_json::to_vec(&args_map).unwrap_or_default())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let started = Instant::now();
    tracing::info!(
        target: "mcp.audit",
        server,
        tool,
        args_sha256 = %args_sha256,
        "mcp call_tool entry"
    );
    manager
        .connect(server)
        .await
        .with_context(|| format!("connect mcp server {server:?}"))?;
    let peer = manager.arc_peer(server).await?;
    ensure_tools_capability(&peer, server)
        .with_context(|| format!("call_tool {tool:?} on {server:?}"))?;
    // Refuse a `call` on a tool that declares execution.taskSupport == "required":
    // the MCP spec mandates such tools be driven through the `tasks` augmentation
    // flow, which openab-agent does not implement. Reject before the wire call so
    // the LLM gets a clear reason instead of a server-side protocol error (rows
    // 492/289). This costs one extra `tools/list` round-trip per call until the
    // planned per-server tools cache (Row 503) lands and can serve the lookup.
    if fetch_tools(manager, server)
        .await?
        .iter()
        .any(|t| t.name.as_ref() == tool && t.task_support() == TaskSupport::Required)
    {
        tracing::info!(
            target: "mcp.audit",
            server,
            tool,
            args_sha256 = %args_sha256,
            duration_ms = started.elapsed().as_millis() as u64,
            outcome = "refused",
            is_error = true,
            "mcp call_tool exit"
        );
        return Err(anyhow!(
            "tool {tool:?} on {server:?} declares taskSupport=\"required\"; openab-agent does not implement the MCP tasks augmentation flow, so this tool cannot be invoked"
        ));
    }
    let timeout = manager.request_timeout(server).await;
    let params = rmcp::model::CallToolRequestParams::new(tool.to_string()).with_arguments(args_map);
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
    let mut options = PeerRequestOptions::no_options();
    options.timeout = Some(timeout);
    // Wire-level Err = transport failure → trips the breaker; wire-level
    // Ok (even with `isError: true`) resets it. See ADR §5.9 / #966 Q2.
    // On timeout rmcp auto-emits notifications/cancelled (reason "request
    // timeout") before surfacing ServiceError::Timeout (ADR §5.6).
    let send_result = async {
        peer.send_request_with_option(request, options)
            .await?
            .await_response()
            .await
    }
    .await;
    let result = match send_result {
        Ok(ServerResult::CallToolResult(r)) => {
            manager.record_tool_call_outcome(server, true);
            r
        }
        Ok(_) => {
            manager.record_tool_call_outcome(server, false);
            tracing::info!(
                target: "mcp.audit",
                server,
                tool,
                args_sha256 = %args_sha256,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "err",
                is_error = true,
                "mcp call_tool exit"
            );
            return Err(anyhow!(
                "call_tool {tool:?} on {server:?}: unexpected non-CallToolResult response"
            ));
        }
        Err(e) => {
            if let ServiceError::Timeout { timeout } = e {
                tracing::info!(
                    target: "mcp.cancel",
                    server,
                    tool,
                    timeout_secs = timeout.as_secs(),
                    "mcp tools/call timed out; sent notifications/cancelled"
                );
            }
            manager.record_tool_call_outcome(server, false);
            tracing::info!(
                target: "mcp.audit",
                server,
                tool,
                args_sha256 = %args_sha256,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "err",
                is_error = true,
                "mcp call_tool exit"
            );
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("call_tool {tool:?} on {server:?}"));
        }
    };
    tracing::info!(
        target: "mcp.audit",
        server,
        tool,
        args_sha256 = %args_sha256,
        duration_ms = started.elapsed().as_millis() as u64,
        outcome = "ok",
        is_error = result.is_error.unwrap_or(false),
        "mcp call_tool exit"
    );
    let is_error = result.is_error;
    let value = serde_json::to_value(&result).context("serialize CallToolResult")?;
    Ok((value, is_error))
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
    ensure_tools_capability(&peer, server)
        .with_context(|| format!("list_tools on {server:?}"))?;
    let timeout = manager.request_timeout(server).await;
    // Manual pagination mirroring rmcp's `list_all_tools`, but per-page
    // bounded by the configured request timeout (ADR §5.6). rmcp's helper
    // takes no options, so we drive `list_tools` ourselves.
    let mut tools = Vec::new();
    // The cursor is an opaque server token (MCP 2025-11-25 pagination): we
    // round-trip `next_cursor` verbatim into the next request's `cursor` and
    // never parse, synthesize, or persist it — its format is the server's
    // private concern and may change between pages.
    let mut cursor = None;
    loop {
        let request = ClientRequest::ListToolsRequest(ListToolsRequest::with_param(
            PaginatedRequestParams::default().with_cursor(cursor),
        ));
        let mut options = PeerRequestOptions::no_options();
        options.timeout = Some(timeout);
        let page = async {
            peer.send_request_with_option(request, options)
                .await?
                .await_response()
                .await
        }
        .await;
        match page {
            Ok(ServerResult::ListToolsResult(result)) => {
                manager.record_tool_call_outcome(server, true);
                tools.extend(result.tools);
                cursor = result.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            Ok(_) => {
                manager.record_tool_call_outcome(server, false);
                return Err(anyhow!("list_tools on {server:?}: unexpected response"));
            }
            Err(e) => {
                if let ServiceError::Timeout { timeout } = e {
                    tracing::info!(
                        target: "mcp.cancel",
                        server,
                        timeout_secs = timeout.as_secs(),
                        "mcp tools/list timed out; sent notifications/cancelled"
                    );
                }
                manager.record_tool_call_outcome(server, false);
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("list_tools on {server:?}"));
            }
        }
    }
    Ok(tools)
}

/// Compact projection of an MCP `Tool` shared by `list_tools` and
/// `describe_tool`. Surfaces the spec metadata an LLM needs to choose a
/// tool: display `title`, behavioural `annotations` hints, and the
/// `task_support` execution mode. `input_schema`/`output_schema`/`icons`
/// are left for `describe_tool` to attach (progressive disclosure).
fn tool_summary(t: &rmcp::model::Tool) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), Value::String(t.name.to_string()));
    if let Some(title) = &t.title {
        map.insert("title".into(), Value::String(title.clone()));
    }
    if let Some(description) = &t.description {
        map.insert("description".into(), Value::String(description.to_string()));
    }
    if let Some(ann) = &t.annotations {
        let mut a = serde_json::Map::new();
        if let Some(v) = &ann.title {
            a.insert("title".into(), Value::String(v.clone()));
        }
        if let Some(v) = ann.read_only_hint {
            a.insert("read_only_hint".into(), Value::Bool(v));
        }
        if let Some(v) = ann.destructive_hint {
            a.insert("destructive_hint".into(), Value::Bool(v));
        }
        if let Some(v) = ann.idempotent_hint {
            a.insert("idempotent_hint".into(), Value::Bool(v));
        }
        if let Some(v) = ann.open_world_hint {
            a.insert("open_world_hint".into(), Value::Bool(v));
        }
        if !a.is_empty() {
            map.insert("annotations".into(), Value::Object(a));
        }
    }
    let support = t.task_support();
    let ts = serde_json::to_value(support).unwrap_or(Value::String("forbidden".into()));
    map.insert("task_support".into(), ts);
    if support == TaskSupport::Required {
        // We do not implement the MCP `tasks` augmentation flow, so a tool that
        // *requires* it cannot be invoked. Mark it unavailable (rather than
        // dropping it) so the LLM sees the tool exists but knows not to call it,
        // and why (rows 289/617). The `call` path enforces the same refusal.
        map.insert("available".into(), Value::Bool(false));
        map.insert(
            "unavailable_reason".into(),
            Value::String("requires task augmentation (not implemented)".into()),
        );
    }
    Value::Object(map)
}

async fn list_tools(manager: &McpRuntimeManager, server: &str) -> Result<Value> {
    let entries: Vec<Value> = fetch_tools(manager, server)
        .await?
        .into_iter()
        .map(|t| tool_summary(&t))
        .collect();
    Ok(Value::Array(entries))
}

async fn describe_tool(manager: &McpRuntimeManager, server: &str, tool: &str) -> Result<Value> {
    // Progressive disclosure (ADR §5.2): `list_tools` returns the compact
    // `tool_summary`; this action adds the full `input_schema` (plus
    // `output_schema`/`icons` when present) for one tool. MCP has no
    // single-tool query, so we list + filter.
    let tool_def = fetch_tools(manager, server)
        .await?
        .into_iter()
        .find(|t| t.name.as_ref() == tool)
        .ok_or_else(|| anyhow!("no tool {tool:?} on mcp server {server:?}"))?;
    let mut summary = tool_summary(&tool_def);
    let obj = summary
        .as_object_mut()
        .expect("tool_summary always returns a JSON object");
    obj.insert(
        "input_schema".into(),
        serde_json::to_value(&tool_def.input_schema)
            .context("serialize tool input_schema")?,
    );
    if let Some(output_schema) = &tool_def.output_schema {
        obj.insert(
            "output_schema".into(),
            serde_json::to_value(output_schema).context("serialize tool output_schema")?,
        );
    }
    if let Some(icons) = &tool_def.icons {
        obj.insert(
            "icons".into(),
            serde_json::to_value(icons).context("serialize tool icons")?,
        );
    }
    Ok(summary)
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
        // `Disconnected` is the cold/idle state — config loaded but the
        // child process hasn't been spawned yet. Lazy connect happens on
        // the first `call` / `list_tools`, so this is NOT a failure mode.
        // Earlier label `"disconnected"` confused LLMs into reporting the
        // server as broken on a plain `list_servers` (PR #959 F1 PoC
        // observation). `"failed"` already covers the error case below.
        ServerStatus::Disconnected => "idle",
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

    #[tokio::test]
    async fn help_returns_doc_string() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let (result, _) = dispatch(&mgr, Action::Help).await.unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains("list_servers"));
        assert!(s.contains("call(server, tool"));
    }

    #[test]
    fn tool_summary_marks_required_task_support_unavailable() {
        use rmcp::model::{Tool, ToolExecution};
        use std::sync::Arc;
        let schema = Arc::new(serde_json::Map::new());

        let required = Tool::new("planner", "long task", schema.clone())
            .with_execution(ToolExecution::new().with_task_support(TaskSupport::Required));
        let v = tool_summary(&required);
        assert_eq!(v["task_support"], Value::String("required".into()));
        assert_eq!(v["available"], Value::Bool(false));
        assert_eq!(
            v["unavailable_reason"],
            Value::String("requires task augmentation (not implemented)".into())
        );

        // No execution metadata => defaults to forbidden and stays available
        // (no diagnostic fields added).
        let plain = Tool::new("echo", "echoes", schema);
        let v2 = tool_summary(&plain);
        assert_eq!(v2["task_support"], Value::String("forbidden".into()));
        assert!(v2.get("available").is_none());
        assert!(v2.get("unavailable_reason").is_none());
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
        let (result, _) = dispatch(&mgr, Action::ListServers).await.unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let by_name: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|e| (e["name"].as_str().unwrap(), e))
            .collect();
        assert_eq!(by_name["fs"]["transport"], "stdio");
        assert_eq!(by_name["fs"]["status"], "idle");
        assert_eq!(by_name["linear"]["transport"], "http");
    }

    #[tokio::test]
    async fn list_servers_empty_yields_empty_array() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let (result, _) = dispatch(&mgr, Action::ListServers).await.unwrap();
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
        let (result, _) = dispatch(&mgr, Action::Status { server: None })
            .await
            .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for e in entries {
            assert_eq!(e["status"], "idle");
            assert!(e["last_error"].is_null());
        }
    }

    #[tokio::test]
    async fn status_labels_failed_servers_with_last_error() {
        // Status uses a `Failed` state distinct from `idle`; the LLM should
        // see the failure surfaced explicitly via `status: "failed"` +
        // `last_error: <msg>` rather than collapsing into `idle`.
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
        // Trip the Failed state via a connect attempt that will fail at spawn.
        let _ = dispatch(
            &mgr,
            Action::Call {
                server: "broken".into(),
                tool: "anything".into(),
                arguments: serde_json::json!({}),
            },
        )
        .await;
        let (result, _) = dispatch(&mgr, Action::Status { server: None })
            .await
            .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["status"], "failed");
        assert!(
            !entries[0]["last_error"].is_null(),
            "Failed status should carry last_error"
        );
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
        let (result, _) = dispatch(
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
        let (result, _) = dispatch(
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
        let (result, _) = dispatch(&mgr, Action::Status { server: None })
            .await
            .unwrap();
        let entries = result.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["status"], "failed");
        let last_error = entries[0]["last_error"].as_str().unwrap();
        assert!(last_error.contains("spawn"), "got: {last_error}");
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

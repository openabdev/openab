//! Single `mcp` meta-tool the LLM sees. See ADR §5.2 + §5.3.
//!
//! Scope: action enum + dispatch wiring + all six actions
//! (`help`, `list_servers`, `list_tools`, `describe_tool`, `call`, `status`).
//! OAuth `login` / `complete_login` are CLI-only (`mcp login`), not
//! LLM-facing meta-tool actions.

use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use rmcp::model::{
    CallToolRequest, ClientRequest, ListToolsRequest, PaginatedRequestParams, ServerResult,
    TaskSupport,
};
use rmcp::service::{PeerRequestOptions, RoleClient, RunningService, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpError;
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
pub async fn dispatch(
    manager: &McpRuntimeManager,
    action: Action,
) -> Result<(Value, Option<bool>)> {
    match action {
        Action::Help => Ok((json!(HELP), None)),
        Action::ListServers => Ok((list_servers(manager).await, None)),
        Action::ListTools { server } => list_tools(manager, &server).await.map(|v| (v, None)),
        Action::DescribeTool { server, tool } => describe_tool(manager, &server, &tool)
            .await
            .map(|v| (v, None)),
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

/// Classify a request-time `ServiceError` as an MCP-server OAuth challenge.
/// rmcp flattens the transport's HTTP 401/403 + `WWW-Authenticate` into
/// `ServiceError::TransportSend(DynamicTransportError)` with the structured
/// `StreamableHttpError` boxed inside, so we downcast to the one concrete
/// transport error type both dial paths use (`StreamableHttpError<reqwest013::Error>`:
/// the OAuth dial wraps `AuthClient<reqwest013::Client>` whose `Error = reqwest013::Error`,
/// and the plain dial uses rmcp's reqwest-0.13 client directly). Returns
/// `Some(required_scope)` on a challenge — the scope from a 403
/// `insufficient_scope`, or `None` for a plain 401 — and `None` when `e` is
/// not an auth challenge.
fn auth_challenge_scope(e: &ServiceError) -> Option<Option<String>> {
    let ServiceError::TransportSend(dyn_err) = e else {
        return None;
    };
    match dyn_err
        .error
        .downcast_ref::<StreamableHttpError<reqwest013::Error>>()?
    {
        StreamableHttpError::AuthRequired(_) => Some(None),
        StreamableHttpError::InsufficientScope(s) => Some(s.required_scope.clone()),
        _ => None,
    }
}

/// Build the caller-facing error for a request-time OAuth challenge. The MCP
/// login flow is interactive (`mcp login <server>`, stdin paste-back), so an
/// automatic in-band reauth-and-retry is impossible here (row 424 ⚠️): the
/// best we can do is set `NeedsAuth` and tell the operator to re-login,
/// surfacing the challenge-provided scope when the server sent one.
fn needs_reauth_error(server: &str, required_scope: &Option<String>) -> anyhow::Error {
    match required_scope {
        Some(scope) if !scope.is_empty() => anyhow!(
            "mcp server {server:?} rejected the request — insufficient scope (server requires {scope:?}); re-authenticate with `mcp login {server} --scope {scope}`"
        ),
        _ => anyhow!(
            "mcp server {server:?} rejected the request (HTTP 401) — (re)authenticate with `mcp login {server}`"
        ),
    }
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
    // Mark this server in-flight for the duration of the call so idle eviction
    // and the concurrency cap never tear out a connection mid-request. The guard
    // decrements the counter on drop, covering every early-return below.
    let _call_guard = manager.begin_call(server).await;
    let peer = manager.arc_peer(server).await?;
    ensure_tools_capability(&peer, server)
        .with_context(|| format!("call_tool {tool:?} on {server:?}"))?;
    // Look up the tool definition once (served from the per-server tools cache,
    // row 503, when warm — normally no extra `tools/list` round-trip) and use it
    // for both pre-flight guards below. Both reject before any wire traffic, so
    // neither must touch the circuit breaker.
    let tools = fetch_tools(manager, server).await?;
    let tool_def = tools.iter().find(|t| t.name.as_ref() == tool);
    // Guard 1 — taskSupport == "required": the MCP spec mandates such tools be
    // driven through the `tasks` augmentation flow, which openab-agent does not
    // implement. Reject so the LLM gets a clear reason instead of a server-side
    // protocol error (rows 492/289).
    if tool_def.map(|t| t.task_support()) == Some(TaskSupport::Required) {
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
    // Guard 2 — validate arguments against the tool's declared `inputSchema`
    // using the schema's own JSON Schema dialect (MCP rows 19-20). A dialect the
    // validator cannot compile, or arguments that violate a compilable schema,
    // are refused here rather than relayed — the model gets the precise reason
    // and can self-correct, and no malformed call reaches the server.
    if let Some(t) = tool_def {
        if let Err(e) = validate_args(t.input_schema.as_ref(), &args_map) {
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
            return Err(e).with_context(|| format!("call_tool {tool:?} on {server:?}"));
        }
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
            // An OAuth challenge (HTTP 401/403) is not a transport fault, so it
            // must not trip the circuit breaker. Flag the server NeedsAuth and
            // return an actionable re-login error instead (row 424).
            if let Some(required_scope) = auth_challenge_scope(&e) {
                manager.mark_needs_auth(server).await;
                tracing::info!(
                    target: "mcp.audit",
                    server,
                    tool,
                    args_sha256 = %args_sha256,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = "auth_required",
                    is_error = true,
                    "mcp call_tool exit"
                );
                return Err(needs_reauth_error(server, &required_scope))
                    .with_context(|| format!("call_tool {tool:?} on {server:?}"));
            }
            // A JSON-RPC error reply (ServiceError::McpError) is a wire-level
            // response, not a transport fault — same class as `isError: true`.
            // It must not trip the breaker or tear down the live client. Mirror
            // the Ok-reset semantics above (ADR §5.9 error model).
            if matches!(e, ServiceError::McpError(_)) {
                manager.record_tool_call_outcome(server, true);
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
            manager.record_tool_call_outcome(server, false);
            // Transport fault: the installed client is dead. Tear it down so the
            // next connect() redials instead of reusing a dead handle via the
            // Connected fast-path (#959 F1). Best-effort — teardown failure must
            // not mask the original transport error returned below.
            let _ = manager.disconnect(server).await;
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
/// `describe_tool` / the `call` task-support guard. Serves from the manager's
/// per-server tools cache when warm (row 503) — the cache is evicted by
/// `OpenabClientHandler::on_tool_list_changed`, so a hit is current. On a miss
/// it paginates `tools/list` and repopulates. The `Arc<RunningService>` clone
/// lets the I/O `.await` run with no runtime lock held.
async fn fetch_tools(manager: &McpRuntimeManager, server: &str) -> Result<Vec<rmcp::model::Tool>> {
    if let Some(cached) = manager.cached_tools(server) {
        return Ok(cached);
    }
    manager
        .connect(server)
        .await
        .with_context(|| format!("connect mcp server {server:?}"))?;
    let _call_guard = manager.begin_call(server).await;
    let peer = manager.arc_peer(server).await?;
    ensure_tools_capability(&peer, server).with_context(|| format!("list_tools on {server:?}"))?;
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
                // OAuth challenge (HTTP 401/403): not a transport fault — don't
                // trip the breaker. Flag NeedsAuth + return a re-login error (row 424).
                if let Some(required_scope) = auth_challenge_scope(&e) {
                    manager.mark_needs_auth(server).await;
                    return Err(needs_reauth_error(server, &required_scope))
                        .with_context(|| format!("list_tools on {server:?}"));
                }
                // JSON-RPC error reply (McpError) = wire-level response, not a
                // transport fault — don't trip the breaker (ADR §5.9).
                if matches!(e, ServiceError::McpError(_)) {
                    manager.record_tool_call_outcome(server, true);
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("list_tools on {server:?}"));
                }
                manager.record_tool_call_outcome(server, false);
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("list_tools on {server:?}"));
            }
        }
    }
    manager.store_tools(server, &tools);
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
    // Only genuine hard-blocks — ones the `call` path actually refuses — set
    // `available:false`. A tool's `$schema` dialect is NOT reflected here: any
    // dialect the validator can compile (draft-07, 2020-12, …) is honoured at
    // the `call` path via `validate_args`, and a dialect it cannot compile is
    // reported only when the tool is actually invoked. Surfacing dialect detail
    // in the LLM-facing summary previously made models decline callable tools
    // (exa/fs draft-07, 2026-06-05), so it stays out of this view.
    let mut blocking: Vec<String> = Vec::new();
    if support == TaskSupport::Required {
        // We do not implement the MCP `tasks` augmentation flow, so a tool that
        // *requires* it cannot be invoked (rows 289/617). The `call` path
        // enforces the same refusal.
        blocking.push("requires task augmentation (not implemented)".into());
    }
    if !blocking.is_empty() {
        map.insert("available".into(), Value::Bool(false));
        map.insert(
            "unavailable_reason".into(),
            Value::String(blocking.join("; ")),
        );
    }
    Value::Object(map)
}

/// Validate a `call`'s `arguments` against the tool's declared `inputSchema`
/// before dispatching `tools/call` (MCP rows 19-20). `validator_for`
/// auto-selects the JSON Schema draft from the schema's `$schema` keyword
/// (draft-07, 2020-12, …; absent ⇒ implied 2020-12 default), so every dialect a
/// server may pin is honoured. A schema whose dialect the validator cannot
/// compile yields the spec's "unsupported dialect" error (row 20); arguments
/// that violate a compilable schema are rejected with every failing path so the
/// model can self-correct (row 19). An empty schema accepts anything.
fn validate_args(
    input_schema: &serde_json::Map<String, Value>,
    args: &serde_json::Map<String, Value>,
) -> Result<()> {
    if input_schema.is_empty() {
        return Ok(());
    }
    let schema = Value::Object(input_schema.clone());
    let validator = jsonschema::validator_for(&schema).map_err(|e| {
        anyhow!(
            "tool inputSchema declares a JSON Schema dialect openab-agent cannot validate ({e}); \
             the dialect is unsupported, so the call is refused (MCP rows 19-20)"
        )
    })?;
    let instance = Value::Object(args.clone());
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| {
            let path = e.instance_path().to_string();
            if path.is_empty() {
                e.to_string()
            } else {
                format!("{path}: {e}")
            }
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "arguments do not satisfy the tool's inputSchema: {}",
            errors.join("; ")
        ))
    }
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
        serde_json::to_value(&tool_def.input_schema).context("serialize tool input_schema")?,
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

    fn transport_send_error(inner: StreamableHttpError<reqwest013::Error>) -> ServiceError {
        use rmcp::transport::DynamicTransportError;
        use std::any::TypeId;
        ServiceError::TransportSend(DynamicTransportError::from_parts(
            "test",
            TypeId::of::<()>(),
            Box::new(inner),
        ))
    }

    #[test]
    fn auth_challenge_scope_classifies_401_403_and_ignores_others() {
        use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};

        // Plain 401 → challenge with no scope.
        let e = transport_send_error(StreamableHttpError::AuthRequired(AuthRequiredError::new(
            "Bearer".into(),
        )));
        assert_eq!(auth_challenge_scope(&e), Some(None));

        // 403 insufficient_scope carrying a scope → challenge with that scope.
        let e = transport_send_error(StreamableHttpError::InsufficientScope(
            InsufficientScopeError::new("Bearer".into(), Some("repo:write".into())),
        ));
        assert_eq!(auth_challenge_scope(&e), Some(Some("repo:write".into())));

        // 403 without a parseable scope → challenge with no scope.
        let e = transport_send_error(StreamableHttpError::InsufficientScope(
            InsufficientScopeError::new("Bearer".into(), None),
        ));
        assert_eq!(auth_challenge_scope(&e), Some(None));

        // A non-auth ServiceError is not a challenge.
        assert_eq!(
            auth_challenge_scope(&ServiceError::Timeout {
                timeout: std::time::Duration::from_secs(1)
            }),
            None
        );
    }

    #[test]
    fn needs_reauth_error_mentions_login_and_scope() {
        let plain = needs_reauth_error("linear", &None).to_string();
        assert!(plain.contains("mcp login linear"), "got: {plain}");
        assert!(plain.contains("401"), "got: {plain}");

        let scoped = needs_reauth_error("linear", &Some("repo:write".into())).to_string();
        assert!(scoped.contains("mcp login linear"), "got: {scoped}");
        assert!(scoped.contains("repo:write"), "got: {scoped}");
        assert!(scoped.contains("insufficient scope"), "got: {scoped}");
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

    #[test]
    fn tool_summary_is_dialect_agnostic() {
        use rmcp::model::Tool;
        use std::sync::Arc;

        // The LLM-facing summary never reflects the `$schema` dialect: dialect
        // handling lives at the `call` path (`validate_args`), not in describe /
        // list. Surfacing it here previously made models decline callable tools
        // (exa/fs draft-07, 2026-06-05).
        let mut d07 = serde_json::Map::new();
        d07.insert(
            "$schema".into(),
            Value::String("http://json-schema.org/draft-07/schema#".into()),
        );
        let foreign = Tool::new("legacy", "old schema", Arc::new(d07));
        let v = tool_summary(&foreign);
        assert!(v.get("available").is_none());
        assert!(v.get("unavailable_reason").is_none());

        // Explicit 2020-12 (with trailing '#') => no diagnostic fields.
        let mut ok = serde_json::Map::new();
        ok.insert(
            "$schema".into(),
            Value::String("https://json-schema.org/draft/2020-12/schema#".into()),
        );
        let v_ok = tool_summary(&Tool::new("modern", "ok", Arc::new(ok)));
        assert!(v_ok.get("available").is_none());

        // Absent $schema => no diagnostic fields.
        let v_absent = tool_summary(&Tool::new("plain", "ok", Arc::new(serde_json::Map::new())));
        assert!(v_absent.get("available").is_none());
    }

    fn schema_map(json: Value) -> serde_json::Map<String, Value> {
        json.as_object().unwrap().clone()
    }

    #[test]
    fn validate_args_empty_schema_accepts_anything() {
        let empty = serde_json::Map::new();
        let mut args = serde_json::Map::new();
        args.insert("anything".into(), json!(42));
        assert!(validate_args(&empty, &args).is_ok());
    }

    #[test]
    fn validate_args_default_dialect_enforces_constraints() {
        // No $schema => implied 2020-12 default (MCP rows 18-21).
        let schema = schema_map(json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "required": ["q"],
            "additionalProperties": false
        }));

        // Valid.
        assert!(validate_args(&schema, &schema_map(json!({ "q": "hi" }))).is_ok());

        // Missing required field.
        let err = validate_args(&schema, &schema_map(json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inputSchema"), "got: {err}");
        assert!(err.contains("q") || err.contains("required"), "got: {err}");

        // Wrong type.
        let err = validate_args(&schema, &schema_map(json!({ "q": 123 })))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inputSchema"), "got: {err}");
    }

    #[test]
    fn validate_args_honours_draft07_dialect() {
        // A server that pins draft-07 must be validated under draft-07, not
        // rejected (the exa/fs case). `validator_for` auto-selects the draft.
        let schema = schema_map(json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "n": { "type": "integer", "minimum": 1 } },
            "required": ["n"]
        }));

        assert!(validate_args(&schema, &schema_map(json!({ "n": 5 }))).is_ok());

        let err = validate_args(&schema, &schema_map(json!({ "n": 0 })))
            .unwrap_err()
            .to_string();
        assert!(err.contains("inputSchema"), "got: {err}");
    }

    #[test]
    fn validate_args_unsupported_dialect_is_refused() {
        // A dialect the validator cannot compile => the spec's "unsupported
        // dialect" error (row 20), surfaced at call time.
        let schema = schema_map(json!({
            "$schema": "https://example.com/no-such-dialect",
            "type": "object"
        }));
        let err = validate_args(&schema, &serde_json::Map::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported"), "got: {err}");
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

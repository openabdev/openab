//! Agent-facing control-plane MCP tools.
//!
//! These four direct tools are intentionally thin clients of the same local
//! Unix-socket API used by `openab agent`: MCP and CLI therefore share one
//! validation/enforcement path and neither sees CP credentials or topology.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use openab_core::control_plane::{LocalClient, LocalRequest, LocalResponse, LocalTarget};
use openab_mcp::mcp::facade::DirectToolProvider;
use openab_mcp::mcp::sources::SessionCtx;
use openab_mcp::rmcp::model::Tool;
use serde_json::{json, Map, Value};

/// Root-side bridge between the facade's token registry and the session pool.
pub struct FacadeRegistrar(pub openab_mcp::mcp::sources::SessionTokens);

impl openab_core::acp_mcp::SessionTokenRegistrar for FacadeRegistrar {
    fn mint(&self, channel_id: &str) -> String {
        self.0.mint(channel_id)
    }

    fn revoke(&self, token: &str) {
        self.0.revoke_token(token)
    }
}

#[derive(Clone)]
pub struct ControlPlaneTools {
    socket_path: PathBuf,
}

impl ControlPlaneTools {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    fn tool(name: &str, description: &str, schema: Value) -> Tool {
        Tool::new(
            name.to_string(),
            description.to_string(),
            Arc::new(
                schema
                    .as_object()
                    .expect("tool schema is an object")
                    .clone(),
            ),
        )
    }

    async fn request(&self, request: LocalRequest) -> Result<LocalResponse> {
        LocalClient::new(self.socket_path.clone())
            .request(&request)
            .await
    }

    fn response(value: LocalResponse) -> Result<(Value, bool)> {
        let is_error = matches!(value, LocalResponse::Error { .. });
        Ok((serde_json::to_value(value)?, is_error))
    }
}

#[async_trait::async_trait]
impl DirectToolProvider for ControlPlaneTools {
    fn provider(&self) -> &str {
        "control-plane"
    }

    fn requires_session(&self) -> bool {
        true
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Self::tool(
                "spawn_agent",
                "Delegate a task to another registered OpenAB agent. Set async=true to return an opaque handle immediately; otherwise wait for the terminal result.",
                json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "minLength": 1},
                        "target": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "minLength": 1},
                                "labels": {"type": "object", "additionalProperties": {"type": "string"}}
                            },
                            "oneOf": [
                                {"required": ["name"], "not": {"required": ["labels"]}},
                                {"required": ["labels"], "not": {"required": ["name"]}}
                            ]
                        },
                        "deadline_secs": {"type": "integer", "minimum": 1, "maximum": 1800, "default": 300},
                        "async": {"type": "boolean", "default": false},
                        "parent": {"type": "string", "description": "Opaque parent delegation handle when spawning a child"}
                    },
                    "required": ["prompt", "target"]
                }),
            ),
            Self::tool(
                "check_delegation",
                "Return the current pending, running, or terminal state of an opaque delegation handle without waiting.",
                json!({
                    "type": "object",
                    "properties": {"handle": {"type": "string", "minLength": 1}},
                    "required": ["handle"]
                }),
            ),
            Self::tool(
                "list_agents",
                "List registered agents in this runtime's control-plane namespace, including labels and current capacity.",
                json!({"type": "object", "additionalProperties": false}),
            ),
            Self::tool(
                "cancel_delegation",
                "Cancel an in-flight delegation using its opaque handle.",
                json!({
                    "type": "object",
                    "properties": {
                        "handle": {"type": "string", "minLength": 1},
                        "reason": {"type": "string", "default": "cancelled by initiating agent"}
                    },
                    "required": ["handle"]
                }),
            ),
        ]
    }

    async fn call(
        &self,
        _ctx: Option<&SessionCtx>,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Result<(Value, bool)> {
        match tool {
            "spawn_agent" => {
                let prompt = args
                    .get("prompt")
                    .and_then(Value::as_str)
                    .context("spawn_agent requires prompt")?;
                let target = args
                    .get("target")
                    .and_then(Value::as_object)
                    .context("spawn_agent requires target")?;
                let name = target
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let labels = target
                    .get("labels")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| {
                                v.as_str()
                                    .map(|s| (k.clone(), s.to_string()))
                                    .context("target label values must be strings")
                            })
                            .collect::<Result<std::collections::BTreeMap<_, _>>>()
                    })
                    .transpose()?;
                let deadline_secs = args
                    .get("deadline_secs")
                    .and_then(Value::as_u64)
                    .unwrap_or(300);
                let async_mode = args.get("async").and_then(Value::as_bool).unwrap_or(false);
                let parent = args
                    .get("parent")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let delegation_id = new_delegation_id();
                let spawned = self
                    .request(LocalRequest::Spawn {
                        delegation_id,
                        target: LocalTarget { name, labels },
                        prompt: prompt.to_string(),
                        deadline_secs,
                        parent,
                    })
                    .await?;
                if async_mode || matches!(spawned, LocalResponse::Error { .. }) {
                    return Self::response(spawned);
                }
                let LocalResponse::Spawned { handle, .. } = spawned else {
                    anyhow::bail!("local API returned a non-spawn response");
                };
                let terminal = tokio::time::timeout(
                    std::time::Duration::from_secs(deadline_secs.saturating_add(5)),
                    self.request(LocalRequest::Await {
                        handle: handle.clone(),
                    }),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("delegation {handle} exceeded the local wait bound")
                })??;
                Self::response(terminal)
            }
            "check_delegation" => {
                let handle = args
                    .get("handle")
                    .and_then(Value::as_str)
                    .context("check_delegation requires handle")?;
                Self::response(
                    self.request(LocalRequest::Check {
                        handle: handle.to_string(),
                    })
                    .await?,
                )
            }
            "list_agents" => Self::response(self.request(LocalRequest::ListAgents).await?),
            "cancel_delegation" => {
                let handle = args
                    .get("handle")
                    .and_then(Value::as_str)
                    .context("cancel_delegation requires handle")?;
                let reason = args
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("cancelled by initiating agent");
                Self::response(
                    self.request(LocalRequest::Cancel {
                        handle: handle.to_string(),
                        reason: reason.to_string(),
                    })
                    .await?,
                )
            }
            other => anyhow::bail!("unknown control-plane tool {other:?}"),
        }
    }
}

fn new_delegation_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("d-{}-{nanos:032x}-{seq:016x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_exactly_the_four_adr_tools() {
        let tools = ControlPlaneTools::new("/tmp/unused.sock".into()).tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "spawn_agent",
                "check_delegation",
                "list_agents",
                "cancel_delegation"
            ]
        );
    }

    #[test]
    fn generated_delegation_ids_are_unique_and_bounded() {
        let a = new_delegation_id();
        let b = new_delegation_id();
        assert_ne!(a, b);
        assert!(a.len() < 96);
        assert!(a.starts_with("d-"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_agents_tool_uses_the_local_socket_api() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let request: LocalRequest = serde_json::from_str(&line).unwrap();
            assert!(matches!(request, LocalRequest::ListAgents));
            let response = LocalResponse::Agents {
                agents: vec![openab_core::control_plane::LocalAgent {
                    name: "worker-1".into(),
                    agent_type: "worker".into(),
                    instance_id: "i-1".into(),
                    labels: Default::default(),
                    active_sessions: 0,
                    max_delegated_sessions: 2,
                }],
            };
            let mut encoded = serde_json::to_string(&response).unwrap();
            encoded.push('\n');
            write.write_all(encoded.as_bytes()).await.unwrap();
        });
        let provider = ControlPlaneTools::new(path);
        assert!(provider.requires_session());
        let (value, is_error) = provider
            .call(
                Some(&SessionCtx {
                    channel_id: "session".into(),
                }),
                "list_agents",
                &Map::new(),
            )
            .await
            .unwrap();
        assert!(!is_error);
        assert_eq!(value["kind"], "agents");
        assert_eq!(value["agents"][0]["name"], "worker-1");
        server.await.unwrap();
    }
}

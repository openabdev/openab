//! Agent-facing control-plane MCP tools.
//!
//! These four direct tools are intentionally thin clients of the same local
//! Unix-socket API used by `openab agent`: MCP and CLI therefore share one
//! validation/enforcement path and neither sees CP credentials or topology.
//!
//! Two pieces of that shared path live here as `pub(crate)` items so the
//! `openab agent` CLI in `main.rs` links the *same* code rather than a parallel
//! copy: [`new_delegation_id`] (the one delegation-id generator) and
//! [`spawn_within_bound`] (the one full-operation deadline wrapper). Keeping a
//! single generator means CLI- and MCP-initiated delegations are
//! indistinguishable on the wire; keeping a single wrapper means both surfaces
//! bound the *whole* spawn — admission plus the optional blocking await —
//! under one `deadline_secs + 5` ceiling instead of leaving the admission
//! round-trip unbounded.

use std::future::Future;
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
                                "labels": {"type": "object", "minProperties": 1, "additionalProperties": {"type": "string"}}
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
                // Bound the WHOLE operation — the admission round-trip and the
                // optional blocking await — under one `deadline_secs + 5`
                // ceiling. Previously only the await was wrapped, leaving the
                // spawn request itself unbounded: a hung admission would block
                // the agent's tool call indefinitely. The `+ 5` is slack over
                // the delegation deadline the server enforces, so a healthy
                // await that runs right up to its deadline is not cut short by
                // this outer guard.
                let spawn = LocalRequest::Spawn {
                    delegation_id,
                    target: LocalTarget { name, labels },
                    prompt: prompt.to_string(),
                    deadline_secs,
                    parent,
                };
                let outcome = spawn_within_bound(deadline_secs, async_mode, spawn, |req| {
                    let this = self.clone();
                    async move { this.request(req).await }
                })
                .await?;
                Self::response(outcome)
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

/// The one delegation-id generator shared by the MCP facade and the
/// `openab agent` CLI. A single generator keeps CLI- and MCP-initiated
/// delegations indistinguishable on the wire (no `d-cli-` vs `d-` fork) while
/// still being process-unique: `pid` separates concurrent processes, the
/// monotonic `seq` separates ids minted inside one process, and the nanosecond
/// clock separates process restarts that reuse a pid.
pub(crate) fn new_delegation_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("d-{}-{nanos:032x}-{seq:016x}", std::process::id())
}

/// Run one full spawn operation under a single `deadline_secs + 5` timeout,
/// shared verbatim by the MCP facade and the `openab agent spawn` CLI.
///
/// "Full operation" is the crux the two surfaces previously diverged on: it is
/// the admission round-trip *and*, unless `async_mode`, the blocking await of
/// the terminal result — both inside **one** [`tokio::time::timeout`]. Bounding
/// only the await (the old MCP behaviour) left a hung admission able to block
/// the caller's tool call forever; bounding neither (the old CLI behaviour)
/// left both unbounded.
///
/// `send` performs one request/response against the local socket; it is a
/// closure so the same wrapper drives the facade's [`LocalClient`] and the
/// CLI's, and so tests can substitute an in-memory transport with no socket.
/// The `+ 5` is deliberate slack above the delegation deadline the server
/// itself enforces, so a healthy await running right up to its deadline is not
/// severed by this outer guard; only a genuinely stuck round-trip trips it.
pub(crate) async fn spawn_within_bound<F, Fut>(
    deadline_secs: u64,
    async_mode: bool,
    spawn: LocalRequest,
    send: F,
) -> Result<LocalResponse>
where
    F: Fn(LocalRequest) -> Fut,
    Fut: Future<Output = Result<LocalResponse>>,
{
    let bound = std::time::Duration::from_secs(deadline_secs.saturating_add(5));
    tokio::time::timeout(bound, async {
        let spawned = send(spawn).await?;
        // Async request, or the admission itself failed: hand back the first
        // response verbatim without awaiting a terminal result.
        if async_mode || matches!(spawned, LocalResponse::Error { .. }) {
            return Ok(spawned);
        }
        let LocalResponse::Spawned { handle, .. } = spawned else {
            anyhow::bail!("local API returned a non-spawn response");
        };
        send(LocalRequest::Await { handle }).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("delegation exceeded the local wait bound of {bound:?}"))?
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
        // One generator: neither surface tags its ids, so CLI- and
        // MCP-initiated delegations are indistinguishable on the wire (no
        // `d-cli-` fork).
        assert!(
            !a.contains("cli"),
            "the shared id must not carry a surface tag"
        );
    }

    /// The `spawn_agent` `target` schema encodes exactly-one-of name/labels,
    /// and a `labels` selector must be non-empty (`minProperties: 1`) so an
    /// empty `{}` object is rejected at the schema boundary rather than
    /// bottoming out in the server's "target.labels is empty" error.
    #[test]
    fn spawn_schema_requires_nonempty_labels_and_exactly_one_target() {
        let tools = ControlPlaneTools::new("/tmp/unused.sock".into()).tools();
        let spawn = tools
            .iter()
            .find(|t| t.name.as_ref() == "spawn_agent")
            .expect("spawn_agent is published");
        let schema = Value::Object((*spawn.input_schema).clone());
        let target = &schema["properties"]["target"];
        assert_eq!(
            target["properties"]["labels"]["minProperties"], 1,
            "an empty labels selector must be rejected by the schema"
        );
        // Exactly-one-of is expressed as oneOf(name-without-labels,
        // labels-without-name).
        let one_of = target["oneOf"].as_array().expect("target uses oneOf");
        assert_eq!(one_of.len(), 2, "exactly two mutually-exclusive shapes");
    }

    /// The shared full-operation wrapper waits for the terminal result in the
    /// blocking (default) case: admission then await, one bound, both
    /// surfaces.
    #[tokio::test]
    async fn spawn_within_bound_blocking_awaits_the_terminal_result() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let seen = calls.clone();
        let spawn = LocalRequest::Spawn {
            delegation_id: "d-1".into(),
            target: LocalTarget {
                name: Some("worker-1".into()),
                labels: None,
            },
            prompt: "hi".into(),
            deadline_secs: 60,
            parent: None,
        };
        let out = spawn_within_bound(60, false, spawn, move |req| {
            let seen = seen.clone();
            async move {
                match req {
                    LocalRequest::Spawn { .. } => {
                        seen.lock().unwrap().push("spawn");
                        Ok(LocalResponse::Spawned {
                            handle: "tok".into(),
                            assigned_to: "prod/worker-1".into(),
                        })
                    }
                    LocalRequest::Await { handle } => {
                        assert_eq!(handle, "tok", "await must use the minted handle");
                        seen.lock().unwrap().push("await");
                        Ok(LocalResponse::Terminal {
                            status: openab_core::control_plane::DelegationStatusWire::Completed,
                            result: Some("done".into()),
                            error: None,
                        })
                    }
                    other => panic!("unexpected request {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(out, LocalResponse::Terminal { .. }));
        assert_eq!(*calls.lock().unwrap(), vec!["spawn", "await"]);
    }

    /// In async mode the wrapper returns the admission handle immediately and
    /// never issues the `await`.
    #[tokio::test]
    async fn spawn_within_bound_async_returns_handle_without_awaiting() {
        let spawn = LocalRequest::Spawn {
            delegation_id: "d-1".into(),
            target: LocalTarget {
                name: Some("worker-1".into()),
                labels: None,
            },
            prompt: "hi".into(),
            deadline_secs: 60,
            parent: None,
        };
        let out = spawn_within_bound(60, true, spawn, move |req| async move {
            match req {
                LocalRequest::Spawn { .. } => Ok(LocalResponse::Spawned {
                    handle: "tok".into(),
                    assigned_to: "prod/worker-1".into(),
                }),
                LocalRequest::Await { .. } => panic!("async mode must not await"),
                other => panic!("unexpected request {other:?}"),
            }
        })
        .await
        .unwrap();
        match out {
            LocalResponse::Spawned { handle, .. } => assert_eq!(handle, "tok"),
            other => panic!("expected Spawned, got {other:?}"),
        }
    }

    /// A hung admission (not merely a hung await) trips the single
    /// `deadline_secs + 5` bound — the regression the combined wrapper closes.
    /// With a paused clock, `tokio::time::timeout` auto-advances to its own
    /// timer once the inner future is stuck pending, so this resolves
    /// instantly instead of waiting six real seconds.
    #[tokio::test(start_paused = true)]
    async fn spawn_within_bound_times_out_a_hung_admission() {
        let spawn = LocalRequest::Spawn {
            delegation_id: "d-1".into(),
            target: LocalTarget {
                name: Some("worker-1".into()),
                labels: None,
            },
            prompt: "hi".into(),
            deadline_secs: 1,
            parent: None,
        };
        // The admission itself never resolves; with only-await bounding this
        // would hang forever. The outer `deadline_secs + 5` guard must fire.
        let err = spawn_within_bound(1, false, spawn, |_req| async {
            std::future::pending::<Result<LocalResponse>>().await
        })
        .await
        .expect_err("a hung admission must time out");
        assert!(
            err.to_string().contains("local wait bound"),
            "unexpected error: {err}"
        );
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

    /// End-to-end over REAL loopback HTTP: the broker serves the OAB MCP
    /// facade (`serve_http_with_tools`) with the four control-plane tools and a
    /// session-token registry; a genuine rmcp Streamable-HTTP client presents a
    /// minted session token in the `Authorization` header, discovers the tools
    /// via `tools/list`, and invokes `list_agents` via `tools/call`. The tool
    /// call is serviced by a FAKE Unix-domain socket standing in for the
    /// runtime's local API. This exercises the whole shipped path — HTTP
    /// transport, session-token resolution, direct-tool dispatch, and the UDS
    /// round-trip — without a control plane.
    #[cfg(unix)]
    #[tokio::test]
    async fn facade_over_http_lists_and_calls_control_plane_tools_with_a_session_token() {
        use openab_mcp::mcp::sources::SessionTokens;
        use openab_mcp::rmcp::model::CallToolRequestParams;
        use openab_mcp::rmcp::transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        };
        use openab_mcp::rmcp::ServiceExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        // 1. Fake UDS backing the ControlPlaneTools provider.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let uds = tokio::spawn(async move {
            // Accept one connection per request (the client opens one per call).
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                if let Ok(Some(line)) = lines.next_line().await {
                    let req: LocalRequest = serde_json::from_str(&line).unwrap();
                    assert!(matches!(req, LocalRequest::ListAgents));
                    let resp = LocalResponse::Agents {
                        agents: vec![openab_core::control_plane::LocalAgent {
                            name: "worker-1".into(),
                            agent_type: "worker".into(),
                            instance_id: "i-1".into(),
                            labels: Default::default(),
                            active_sessions: 0,
                            max_delegated_sessions: 2,
                        }],
                    };
                    let mut encoded = serde_json::to_string(&resp).unwrap();
                    encoded.push('\n');
                    let _ = write.write_all(encoded.as_bytes()).await;
                    let _ = write.flush().await;
                }
            }
        });

        // 2. Session-token registry; mint one for our channel.
        let tokens = SessionTokens::new();
        let session_token = tokens.mint("session-channel");

        // 3. Serve the facade on a discovered free loopback port.
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let providers: Vec<Arc<dyn DirectToolProvider>> =
            vec![Arc::new(ControlPlaneTools::new(sock.clone()))];
        let facade_tokens = tokens.clone();
        let facade = tokio::spawn(async move {
            let _ = openab_mcp::mcp::facade::serve_http_with_tools(
                &addr.to_string(),
                Vec::new(),
                providers,
                facade_tokens,
            )
            .await;
        });
        // Wait for the listener to come up.
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // 4. Real rmcp Streamable-HTTP client carrying the session token.
        let mut headers: std::collections::HashMap<
            reqwest013::header::HeaderName,
            reqwest013::header::HeaderValue,
        > = std::collections::HashMap::new();
        headers.insert(
            reqwest013::header::AUTHORIZATION,
            format!("Bearer {session_token}").parse().unwrap(),
        );
        let http = reqwest013::Client::builder().build().unwrap();
        let cfg = StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .custom_headers(headers);
        let transport = StreamableHttpClientTransport::with_client(http, cfg);
        let client = ().serve(transport).await.expect("mcp handshake");

        // tools/list must include the four control-plane tools.
        let listed = client.list_all_tools().await.expect("tools/list");
        let names: std::collections::BTreeSet<String> =
            listed.iter().map(|t| t.name.to_string()).collect();
        for expected in [
            "spawn_agent",
            "check_delegation",
            "list_agents",
            "cancel_delegation",
        ] {
            assert!(names.contains(expected), "tools/list missing {expected}");
        }

        // tools/call list_agents → routed through the fake UDS.
        let called = client
            .call_tool(
                CallToolRequestParams::new("list_agents").with_arguments(serde_json::Map::new()),
            )
            .await
            .expect("tools/call list_agents");
        assert_ne!(called.is_error, Some(true), "list_agents must succeed");
        let payload = serde_json::to_value(&called).unwrap();
        let text = payload.to_string();
        assert!(
            text.contains("worker-1"),
            "the UDS-backed roster must reach the caller: {text}"
        );

        client.cancel().await.ok();
        facade.abort();
        uds.abort();
    }
}

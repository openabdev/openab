//! Phase 6.4.x — production builder end-to-end contract test.
//!
//! Spins up a tiny local HTTP server, builds a real
//! ``HeartbeatProducer`` via :meth:`HeartbeatProducer::build_production`,
//! and verifies the production transport POSTs to the canonical
//! AAP heartbeat URL with the right path, the right
//! ``Authorization: Bearer`` header, and the structured request
//! body. The server captures every request so the assertions can
//! inspect path / header / body without scraping logs.
//!
//! Required evidence for Blocker 1 (production composition):
//!
//! * The production-builder producer is wired into a real
//!   ``tokio::spawn`` heartbeat loop.
//! * It POSTs to
//!   ``http://<local>/v1/integrations/openab/agent/heartbeat``.
//! * The ``Authorization`` header carries the bearer token.
//! * The body decodes to the structured
//!   ``AgentLeaseHeartbeatRequest`` with all six identity fields.
//!
//! The server is bound to a free OS-assigned port (``127.0.0.1:0``)
//! so the test never collides with a real deployment. The
//! ``tokio::sync::oneshot`` channel keeps the server alive until
//! the test reads the captured request.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use openab_core::admission::NativeWorkflowMetadata;
use openab_core::agent_lease_heartbeat::{HeartbeatProducer, ResolvedHeartbeatConfig};

/// Captured HTTP request — method, path, headers, body. The local
/// server stores exactly one capture per test for deterministic
/// assertions.
#[derive(Debug, Default, Clone)]
struct CapturedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: String,
}

/// Spin up a local HTTP server that captures one request and
/// responds with a structured ACCEPTED body. Returns the bound
/// address and the ``Arc<Mutex<CapturedRequest>>`` that the
/// handler fills in.
async fn spawn_local_capture_server() -> (
    SocketAddr,
    Arc<Mutex<CapturedRequest>>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test must bind a free local port");
    let addr = listener.local_addr().expect("local_addr is available");
    let captured = Arc::new(Mutex::new(CapturedRequest::default()));
    let captured_for_handler = Arc::clone(&captured);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    return;
                }
                accept = listener.accept() => {
                    let (mut socket, _peer) = match accept {
                        Ok(pair) => pair,
                        Err(_) => continue,
                    };
                    let cap = Arc::clone(&captured_for_handler);
                    tokio::spawn(async move {
                        let mut buffer = Vec::with_capacity(4096);
                        // Read until the body delimiter is consumed.
                        loop {
                            let mut chunk = vec![0u8; 1024];
                            let n = match socket.read(&mut chunk).await {
                                Ok(0) => break,
                                Ok(n) => n,
                                Err(_) => return,
                            };
                            buffer.extend_from_slice(&chunk[..n]);
                            if let Some(idx) = find_body_start(&buffer) {
                                let header_block = &buffer[..idx];
                                let header_str = String::from_utf8_lossy(header_block);
                                let mut content_length = 0usize;
                                let mut method = String::new();
                                let mut path = String::new();
                                let mut authorization = None;
                                let mut content_type = None;
                                for line in header_str.lines() {
                                    if method.is_empty() && line.starts_with("POST ") {
                                        let mut parts = line.split_whitespace();
                                        method = parts.next().unwrap_or("").to_string();
                                        path = parts.next().unwrap_or("").to_string();
                                    } else if let Some((_, value)) = line.split_once(':') {
                                        let key = line
                                            .get(..line.len() - value.len() - 1)
                                            .unwrap_or("")
                                            .to_ascii_lowercase();
                                        let trimmed = value.trim();
                                        if key == "authorization" {
                                            authorization = Some(trimmed.to_string());
                                        } else if key == "content-type" {
                                            content_type = Some(trimmed.to_string());
                                        } else if key == "content-length" {
                                            content_length = trimmed.parse().unwrap_or(0);
                                        }
                                    }
                                }
                                // Read remaining body bytes up to content_length.
                                let body_start = idx + 4; // "\r\n\r\n"
                                loop {
                                    let have = buffer.len().saturating_sub(body_start);
                                    if have >= content_length {
                                        break;
                                    }
                                    let mut chunk = vec![0u8; 1024];
                                    let n = match socket.read(&mut chunk).await {
                                        Ok(0) => break,
                                        Ok(n) => n,
                                        Err(_) => return,
                                    };
                                    buffer.extend_from_slice(&chunk[..n]);
                                }
                                let body = String::from_utf8_lossy(
                                    &buffer[body_start..body_start + content_length.min(buffer.len() - body_start)],
                                )
                                .to_string();
                                {
                                    let mut g = cap.lock().await;
                                    g.method = method;
                                    g.path = path;
                                    g.authorization = authorization;
                                    g.content_type = content_type;
                                    g.body = body;
                                }
                                // Reply with a structured ACCEPTED body so
                                // reqwest sees a 2xx and the producer does
                                // not retry.
                                let response_body = r#"{"disposition":"ACCEPTED","reason":"RENEWED","lease_id":"lease-1","generation":1,"expires_at":"2026-09-01T00:00:00+00:00"}"#;
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    response_body.len(),
                                    response_body,
                                );
                                let _ = socket.write_all(response.as_bytes()).await;
                                let _ = socket.shutdown().await;
                                return;
                            }
                        }
                    });
                }
            }
        }
    });

    (addr, captured, shutdown_tx)
}

fn find_body_start(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn metadata(dispatch_id: &str) -> NativeWorkflowMetadata {
    NativeWorkflowMetadata {
        dispatch_id: dispatch_id.into(),
        conversation_key: "discord:c:1".into(),
        workflow_run_id: "wfr-test-1".into(),
        task_id: "task-1".into(),
        role: "PRIMARY".into(),
        agent: "ArthurClaude".into(),
        lease_id: "lease-test-1".into(),
        lease_generation: 1,
        expected_revision: 1,
        language: Some("en".into()),
        project_id: Some("arthur-ai-platform".into()),
        project_root: Some("/tmp/proj".into()),
        native_execution_session_key: Some("native-dispatch:ArthurClaude:dispatch-test-1".into()),
        transport: Some("DISCORD".into()),
        delivery_destination: None,
        scope_policy: None,
    }
}

#[tokio::test]
async fn production_builder_posts_to_canonical_aap_heartbeat_path() {
    let (addr, captured, shutdown_tx) = spawn_local_capture_server().await;

    let resolved = ResolvedHeartbeatConfig {
        aap_runtime_url: format!("http://{addr}"),
        bearer_token: "test-bearer-token-xyz".into(),
        heartbeat_interval_seconds: 60,
        request_timeout_seconds: 2,
        retry_max: 0,
        retry_backoff_ms: 1,
        ttl_seconds: None,
    };
    let producer = HeartbeatProducer::build_production(resolved)
        .expect("production builder must succeed when the credential is present");

    let handle = producer.start(&metadata("dispatch-test-1"));

    // Wait long enough for the first immediate tick to land.
    tokio::time::sleep(Duration::from_millis(500)).await;

    handle.stop().await;
    // Give the server a moment to flush the response.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = shutdown_tx.send(());

    let captured = captured.lock().await.clone();
    assert_eq!(
        captured.method, "POST",
        "production builder must POST; got method={}",
        captured.method
    );
    assert_eq!(
        captured.path, "/v1/integrations/openab/agent/heartbeat",
        "production builder must POST to the canonical AAP heartbeat path"
    );
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer test-bearer-token-xyz"),
        "Authorization header must carry the configured bearer token"
    );
    assert!(
        captured
            .content_type
            .as_deref()
            .unwrap_or("")
            .starts_with("application/json"),
        "Content-Type must be application/json; got {:?}",
        captured.content_type
    );

    let payload: Value =
        serde_json::from_str(&captured.body).expect("heartbeat body must be valid JSON");
    assert_eq!(payload["workflow_run_id"], "wfr-test-1");
    assert_eq!(payload["lease_id"], "lease-test-1");
    assert_eq!(payload["lease_generation"], 1);
    assert_eq!(payload["agent"], "ArthurClaude");
    assert_eq!(payload["role"], "PRIMARY");
    assert_eq!(payload["dispatch_id"], "dispatch-test-1");
}

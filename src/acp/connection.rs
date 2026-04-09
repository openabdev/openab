use crate::acp::protocol::{JsonRpcMessage, JsonRpcRequest, JsonRpcResponse};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

fn expand_env(val: &str) -> String {
    if val.starts_with("${") && val.ends_with('}') {
        let key = &val[2..val.len() - 1];
        std::env::var(key).unwrap_or_default()
    } else {
        val.to_string()
    }
}
use tokio::time::Instant;

/// A content block for the ACP prompt — either text or image.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text { text: String },
    Image { media_type: String, data: String },
}

impl ContentBlock {
    pub fn to_json(&self) -> Value {
        match self {
            ContentBlock::Text { text } => json!({
                "type": "text",
                "text": text
            }),
            ContentBlock::Image { media_type, data } => json!({
                "type": "image",
                "data": data,
                "mimeType": media_type
            }),
        }
    }
}

pub struct AcpConnection {
    _proc: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>>,
    notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>>,
    pub acp_session_id: Option<String>,
    pub last_active: Instant,
    pub session_reset: bool,
    _reader_handle: JoinHandle<()>,
}

impl AcpConnection {
    pub async fn spawn(
        command: &str,
        args: &[String],
        working_dir: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<Self> {
        info!(cmd = command, ?args, cwd = working_dir, "spawning agent");

        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .current_dir(working_dir)
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, expand_env(v));
        }
        let mut proc = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to spawn {command}: {e}"))?;

        let stdout = proc.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stdin = proc.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdin = Arc::new(Mutex::new(stdin));

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let notify_tx: Arc<Mutex<Option<mpsc::UnboundedSender<JsonRpcMessage>>>> =
            Arc::new(Mutex::new(None));

        let reader_handle = {
            let pending = pending.clone();
            let notify_tx = notify_tx.clone();
            let stdin_clone = stdin.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {}
                        Err(e) => {
                            error!("reader error: {e}");
                            break;
                        }
                    }
                    let msg: JsonRpcMessage = match serde_json::from_str(line.trim()) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    debug!(line = line.trim(), "acp_recv");

                    // Auto-reply session/request_permission
                    if msg.method.as_deref() == Some("session/request_permission") {
                        if let Some(id) = msg.id {
                            let title = msg.params.as_ref()
                                .and_then(|p| p.get("toolCall"))
                                .and_then(|t| t.get("title"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("?");
                            info!(title, "auto-allow permission");
                            let reply = JsonRpcResponse::new(id, json!({"optionId": "allow_always"}));
                            if let Ok(data) = serde_json::to_string(&reply) {
                                let mut w = stdin_clone.lock().await;
                                let _ = w.write_all(format!("{data}\n").as_bytes()).await;
                                let _ = w.flush().await;
                            }
                        }
                        continue;
                    }

                    // Response (has id) → resolve pending AND forward to subscriber
                    if let Some(id) = msg.id {
                        let mut map = pending.lock().await;
                        if let Some(tx) = map.remove(&id) {
                            // Forward to subscriber so they see the completion
                            let sub = notify_tx.lock().await;
                            if let Some(ntx) = sub.as_ref() {
                                // Clone the essential fields for the subscriber
                                let _ = ntx.send(JsonRpcMessage {
                                    id: Some(id),
                                    method: None,
                                    result: msg.result.clone(),
                                    error: msg.error.clone(),
                                    params: None,
                                });
                            }
                            let _ = tx.send(msg);
                            continue;
                        }
                    }

                    // Notification → forward to subscriber
                    let sub = notify_tx.lock().await;
                    if let Some(tx) = sub.as_ref() {
                        let _ = tx.send(msg);
                    }
                }

                // Connection closed — resolve all pending with error
                let mut map = pending.lock().await;
                for (_, tx) in map.drain() {
                    let _ = tx.send(JsonRpcMessage {
                        id: None,
                        method: None,
                        result: None,
                        error: Some(crate::acp::protocol::JsonRpcError {
                            code: -1,
                            message: "connection closed".into(),
                        }),
                        params: None,
                    });
                }
                // Signal subscriber
                let sub = notify_tx.lock().await;
                drop(sub);
            })
        };

        Ok(Self {
            _proc: proc,
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            notify_tx,
            acp_session_id: None,
            last_active: Instant::now(),
            session_reset: false,
            _reader_handle: reader_handle,
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn send_raw(&self, data: &str) -> Result<()> {
        debug!(data = data.trim(), "acp_send");
        let mut w = self.stdin.lock().await;
        w.write_all(data.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<JsonRpcMessage> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(id, method, params);
        let data = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        self.send_raw(&data).await?;

        let timeout_secs = if method == "session/new" { 120 } else { 30 };
        let resp = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
            .await
            .map_err(|_| anyhow!("timeout waiting for {method} response"))?
            .map_err(|_| anyhow!("channel closed waiting for {method}"))?;

        if let Some(err) = &resp.error {
            return Err(anyhow!("{err}"));
        }
        Ok(resp)
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let resp = self
            .send_request(
                "initialize",
                Some(json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "openab", "version": "0.1.0"},
                })),
            )
            .await?;

        let agent_name = resp.result.as_ref()
            .and_then(|r| r.get("agentInfo"))
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");
        info!(agent = agent_name, "initialized");
        Ok(())
    }

    pub async fn session_new(&mut self, cwd: &str) -> Result<String> {
        let resp = self
            .send_request(
                "session/new",
                Some(json!({"cwd": cwd, "mcpServers": []})),
            )
            .await?;

        let session_id = resp.result.as_ref()
            .and_then(|r| r.get("sessionId"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("no sessionId in session/new response"))?
            .to_string();

        info!(session_id = %session_id, "session created");
        self.acp_session_id = Some(session_id.clone());
        Ok(session_id)
    }

    /// Send a prompt with content blocks (text and/or images) and return a receiver
    /// for streaming notifications. The final message on the channel will have id set
    /// (the prompt response).
    pub async fn session_prompt(
        &mut self,
        content_blocks: Vec<ContentBlock>,
    ) -> Result<(mpsc::UnboundedReceiver<JsonRpcMessage>, u64)> {
        self.last_active = Instant::now();

        let session_id = self
            .acp_session_id
            .as_ref()
            .ok_or_else(|| anyhow!("no session"))?;

        let (tx, rx) = mpsc::unbounded_channel();
        *self.notify_tx.lock().await = Some(tx);

        let id = self.next_id();

        // Convert content blocks to JSON
        let prompt_json: Vec<Value> = content_blocks
            .iter()
            .map(|b| b.to_json())
            .collect();

        let req = JsonRpcRequest::new(
            id,
            "session/prompt",
            Some(json!({
                "sessionId": session_id,
                "prompt": prompt_json,
            })),
        );
        let data = serde_json::to_string(&req)?;

        let (resp_tx, _resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, resp_tx);

        self.send_raw(&data).await?;
        Ok((rx, id))
    }

    /// Call after prompt streaming is done to clean up subscriber.
    pub async fn prompt_done(&mut self) {
        *self.notify_tx.lock().await = None;
        self.last_active = Instant::now();
    }

    pub fn alive(&self) -> bool {
        !self._reader_handle.is_finished()
    }
}

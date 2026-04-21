use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
use crate::config;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

const LINE_API: &str = "https://api.line.me/v2/bot";

// --- LineAdapter: implements ChatAdapter for LINE Messaging API ---

pub struct LineAdapter {
    client: reqwest::Client,
    channel_access_token: String,
}

impl LineAdapter {
    pub fn new(channel_access_token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            channel_access_token,
        }
    }

    async fn api_post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .client
            .post(format!("{LINE_API}/{path}"))
            .header("Authorization", format!("Bearer {}", self.channel_access_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            let msg = json["message"].as_str().unwrap_or("unknown error");
            return Err(anyhow!("LINE API {path}: {status} {msg}"));
        }
        Ok(json)
    }

    /// Get user profile to resolve display name.
    async fn get_profile(&self, user_id: &str) -> Option<String> {
        let resp = self
            .client
            .get(format!("{LINE_API}/profile/{user_id}"))
            .header("Authorization", format!("Bearer {}", self.channel_access_token))
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        json["displayName"].as_str().map(|s| s.to_string())
    }
}

#[async_trait]
impl ChatAdapter for LineAdapter {
    fn platform(&self) -> &'static str {
        "line"
    }

    fn message_limit(&self) -> usize {
        5000
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let resp = self
            .api_post(
                "message/push",
                serde_json::json!({
                    "to": channel.channel_id,
                    "messages": [{ "type": "text", "text": content }]
                }),
            )
            .await?;
        let msg_id = resp["sentMessages"][0]["id"]
            .as_str()
            .or_else(|| resp["sentMessages"][0]["id"].as_u64().map(|_| "0"))
            .unwrap_or("0")
            .to_string();
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: msg_id,
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        _title: &str,
    ) -> Result<ChannelRef> {
        // LINE doesn't have threads — continue in the same chat
        Ok(channel.clone())
    }

    async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        // LINE doesn't support reactions on messages
        Ok(())
    }

    async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }
}

// --- Webhook signature validation ---

fn validate_signature(channel_secret: &str, body: &[u8], signature: &str) -> bool {
    use base64::Engine;
    let key = hmac_sha256::HMAC::mac(body, channel_secret.as_bytes());
    let expected = base64::engine::general_purpose::STANDARD.encode(key);
    expected == signature
}

// --- Minimal HTTP webhook server ---

#[allow(clippy::too_many_arguments)]
pub async fn run_line_adapter(
    channel_access_token: String,
    channel_secret: String,
    webhook_port: u16,
    allow_all_users: bool,
    allow_all_groups: bool,
    allowed_users: HashSet<String>,
    allowed_groups: HashSet<String>,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let adapter = Arc::new(LineAdapter::new(channel_access_token));
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{webhook_port}")).await?;
    info!(port = webhook_port, "LINE webhook server listening");

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (mut stream, addr) = match accept {
                    Ok(v) => v,
                    Err(e) => { error!("accept error: {e}"); continue; }
                };
                let secret = channel_secret.clone();
                let adapter = adapter.clone();
                let router = router.clone();
                let allowed_users = allowed_users.clone();
                let allowed_groups = allowed_groups.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(
                        &mut stream, &secret, &adapter, &router,
                        allow_all_users, allow_all_groups,
                        &allowed_users, &allowed_groups,
                    ).await {
                        debug!(addr = %addr, error = %e, "webhook request error");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                info!("LINE adapter shutting down");
                return Ok(());
            }
        }
    }
}

async fn handle_connection(
    stream: &mut tokio::net::TcpStream,
    channel_secret: &str,
    adapter: &Arc<LineAdapter>,
    router: &Arc<AdapterRouter>,
    allow_all_users: bool,
    allow_all_groups: bool,
    allowed_users: &HashSet<String>,
    allowed_groups: &HashSet<String>,
) -> Result<()> {
    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).await?;
    buf.truncate(n);
    let raw = String::from_utf8_lossy(&buf);

    // Parse HTTP request: extract headers and body
    let (headers_part, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));

    // Extract signature header (case-insensitive)
    let signature = headers_part
        .lines()
        .find_map(|line| {
            let lower = line.to_lowercase();
            if lower.starts_with("x-line-signature:") {
                Some(line.splitn(2, ':').nth(1)?.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    // Always respond 200 to LINE platform
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    stream.write_all(response.as_bytes()).await?;

    if body.is_empty() {
        return Ok(());
    }

    // Validate signature
    if !validate_signature(channel_secret, body.as_bytes(), &signature) {
        warn!("invalid LINE webhook signature");
        return Ok(());
    }

    let payload: serde_json::Value = serde_json::from_str(body)?;
    let events = payload["events"].as_array().cloned().unwrap_or_default();

    for event in events {
        let event_type = event["type"].as_str().unwrap_or("");
        if event_type != "message" {
            continue;
        }
        let msg_type = event["message"]["type"].as_str().unwrap_or("");
        if msg_type != "text" {
            continue;
        }

        let source_type = event["source"]["type"].as_str().unwrap_or("");
        let user_id = event["source"]["userId"].as_str().unwrap_or("").to_string();
        let text = event["message"]["text"].as_str().unwrap_or("").to_string();
        let msg_id = event["message"]["id"].as_str().unwrap_or("0").to_string();

        if text.is_empty() || user_id.is_empty() {
            continue;
        }

        // Determine channel_id: for groups/rooms use groupId/roomId, for 1:1 use userId
        let (channel_id, is_group) = match source_type {
            "group" => {
                let gid = event["source"]["groupId"].as_str().unwrap_or("").to_string();
                if !allow_all_groups && !allowed_groups.contains(&gid) {
                    debug!(group_id = %gid, "denied LINE group, ignoring");
                    continue;
                }
                (gid, true)
            }
            "room" => {
                let rid = event["source"]["roomId"].as_str().unwrap_or("").to_string();
                if !allow_all_groups && !allowed_groups.contains(&rid) {
                    debug!(room_id = %rid, "denied LINE room, ignoring");
                    continue;
                }
                (rid, true)
            }
            _ => {
                // user (1:1 chat)
                if !allow_all_users && !allowed_users.contains(&user_id) {
                    debug!(user_id = %user_id, "denied LINE user, ignoring");
                    continue;
                }
                (user_id.clone(), false)
            }
        };

        // For group chats, also check user allowlist
        if is_group && !allow_all_users && !allowed_users.contains(&user_id) {
            debug!(user_id = %user_id, "denied LINE user in group, ignoring");
            continue;
        }

        let display_name = adapter
            .get_profile(&user_id)
            .await
            .unwrap_or_else(|| user_id.clone());

        let sender = SenderContext {
            schema: "openab.sender.v1".into(),
            sender_id: user_id.clone(),
            sender_name: display_name.clone(),
            display_name,
            channel: "line".into(),
            channel_id: channel_id.clone(),
            thread_id: None,
            is_bot: false,
        };
        let sender_json = serde_json::to_string(&sender).unwrap();

        let channel_ref = ChannelRef {
            platform: "line".into(),
            channel_id: channel_id.clone(),
            thread_id: None,
            parent_id: None,
        };

        let trigger_msg = MessageRef {
            channel: channel_ref.clone(),
            message_id: msg_id.clone(),
        };

        let adapter_dyn: Arc<dyn ChatAdapter> = adapter.clone();
        let router = router.clone();
        tokio::spawn(async move {
            if let Err(e) = router
                .handle_message(
                    &adapter_dyn,
                    &channel_ref,
                    &sender_json,
                    &text,
                    Vec::new(),
                    &trigger_msg,
                )
                .await
            {
                error!("LINE handle_message error: {e}");
            }
        });
    }

    Ok(())
}

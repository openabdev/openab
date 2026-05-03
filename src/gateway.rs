use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef, SenderContext};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

// --- Gateway event/reply schemas (mirrors gateway service) ---

#[derive(Clone, Debug, Deserialize)]
struct GatewayEvent {
    #[allow(dead_code)]
    schema: String,
    event_id: String,
    #[allow(dead_code)]
    timestamp: String,
    platform: String,
    channel: GwChannel,
    sender: GwSender,
    content: GwContent,
    #[serde(default)]
    #[allow(dead_code)]
    mentions: Vec<String>,
    message_id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GwChannel {
    id: String,
    #[serde(rename = "type")]
    channel_type: String,
    thread_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GwSender {
    id: String,
    name: String,
    display_name: String,
    is_bot: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GwContent {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Serialize)]
struct GatewayReply {
    schema: String,
    reply_to: String,
    platform: String,
    channel: ReplyChannel,
    content: ReplyContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

#[derive(Serialize)]
struct ReplyChannel {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
}

#[derive(Serialize)]
struct ReplyContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GatewayResponse {
    #[allow(dead_code)]
    schema: String,
    request_id: String,
    success: bool,
    thread_id: Option<String>,
    message_id: Option<String>,
    error: Option<String>,
}

// --- GatewayAdapter: ChatAdapter over WebSocket ---

type PendingRequests = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<GatewayResponse>>>>;
type SharedWsTx = Arc<Mutex<futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Message,
>>>;

pub struct GatewayAdapter {
    ws_tx: SharedWsTx,
    pending: PendingRequests,
    platform_name: &'static str,
    streaming: bool,
}

impl GatewayAdapter {
    fn new(
        ws_tx: SharedWsTx,
        pending: PendingRequests,
        platform_name: &'static str,
        streaming: bool,
    ) -> Self {
        Self {
            ws_tx,
            pending,
            platform_name,
            streaming,
        }
    }
}

/// Send a fire-and-forget reply via the shared WebSocket (no request-response).
/// Used for slash command responses where we don't need message_id back.
async fn send_fire_and_forget(
    ws_tx: &SharedWsTx,
    channel: &ChannelRef,
    content: &str,
) -> Result<()> {
    let reply = GatewayReply {
        schema: "openab.gateway.reply.v1".into(),
        reply_to: channel.origin_event_id.clone().unwrap_or_default(),
        platform: channel.platform.clone(),
        channel: ReplyChannel {
            id: channel.channel_id.clone(),
            thread_id: channel.thread_id.clone(),
        },
        content: ReplyContent {
            content_type: "text".into(),
            text: content.into(),
        },
        command: None,
        request_id: None,
    };
    let json = serde_json::to_string(&reply)?;
    ws_tx.lock().await.send(Message::Text(json)).await?;
    Ok(())
}

#[async_trait]
impl ChatAdapter for GatewayAdapter {
    fn platform(&self) -> &'static str {
        self.platform_name
    }

    fn message_limit(&self) -> usize {
        4096 // Telegram limit
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let req_id = if self.streaming {
            Some(format!("req_{}", uuid::Uuid::new_v4()))
        } else {
            None
        };

        let pending_rx = if let Some(ref id) = req_id {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.pending.lock().await.insert(id.clone(), tx);
            Some(rx)
        } else {
            None
        };

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: channel.origin_event_id.clone().unwrap_or_default(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: content.into(),
            },
            command: None,
            request_id: req_id.clone(),
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;

        // When streaming is enabled, wait for gateway to return real message_id
        // (needed for edit_message). Otherwise fire-and-forget.
        let msg_id = if let (Some(rx), Some(ref id)) = (pending_rx, &req_id) {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                Ok(Ok(resp)) if resp.success => resp.message_id.unwrap_or_else(|| "gw_sent".into()),
                _ => {
                    self.pending.lock().await.remove(id);
                    "gw_sent".into()
                }
            }
        } else {
            "gw_sent".into()
        };

        Ok(MessageRef {
            channel: channel.clone(),
            message_id: msg_id,
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        // Send create_topic command to gateway
        let req_id = format!("req_{}", uuid::Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(req_id.clone(), tx);

        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: String::new(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: None,
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: title.into(),
            },
            command: Some("create_topic".into()),
            request_id: Some(req_id.clone()),
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;

        // Wait for response (5s timeout)
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(resp)) if resp.success => Ok(ChannelRef {
                platform: channel.platform.clone(),
                channel_id: channel.channel_id.clone(),
                thread_id: resp.thread_id,
                parent_id: None,
                origin_event_id: channel.origin_event_id.clone(),
            }),
            Ok(Ok(resp)) => {
                warn!(err = ?resp.error, "create_topic failed, falling back to same channel");
                Ok(channel.clone())
            }
            _ => {
                warn!("create_topic timeout, falling back to same channel");
                self.pending.lock().await.remove(&req_id);
                Ok(channel.clone())
            }
        }
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: emoji.into(),
            },
            command: Some("add_reaction".into()),
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: emoji.into(),
            },
            command: Some("remove_reaction".into()),
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: msg.message_id.clone(),
            platform: msg.channel.platform.clone(),
            channel: ReplyChannel {
                id: msg.channel.channel_id.clone(),
                thread_id: msg.channel.thread_id.clone(),
            },
            content: ReplyContent {
                content_type: "text".into(),
                text: content.into(),
            },
            command: Some("edit_message".into()),
            request_id: None,
        };
        let json = serde_json::to_string(&reply)?;
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
        Ok(())
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        self.streaming
    }
}

// --- Run the gateway adapter (connects to gateway WS, routes events to AdapterRouter) ---

/// Resolved gateway configuration passed to the adapter at startup.
pub struct GatewayParams {
    pub url: String,
    pub platform: String,
    pub token: Option<String>,
    pub bot_username: Option<String>,
    pub allow_all_channels: bool,
    pub allowed_channels: Vec<String>,
    pub allow_all_users: bool,
    pub allowed_users: Vec<String>,
    pub streaming: bool,
}

pub async fn run_gateway_adapter(
    params: GatewayParams,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let platform: &'static str = Box::leak(params.platform.into_boxed_str());

    // Append auth token as query param if configured
    let gateway_url = params.url;
    let bot_username = params.bot_username;
    let allow_all_channels = params.allow_all_channels;
    let allowed_channels = params.allowed_channels;
    let allow_all_users = params.allow_all_users;
    let allowed_users = params.allowed_users;
    let streaming = params.streaming;

    let connect_url = match &params.token {
        Some(token) => {
            let sep = if gateway_url.contains('?') { "&" } else { "?" };
            format!("{gateway_url}{sep}token={token}")
        }
        None => {
            warn!("gateway.token not set — WebSocket connection is NOT authenticated");
            gateway_url.clone()
        }
    };
    let mut backoff_secs = 1u64;
    const MAX_BACKOFF: u64 = 30;

    loop {
        // Check shutdown before connecting
        if *shutdown_rx.borrow() {
            info!("gateway adapter shutting down");
            return Ok(());
        }

        info!(url = %gateway_url, "connecting to custom gateway");

        let ws_stream = match tokio_tungstenite::connect_async(&connect_url).await {
            Ok((stream, _)) => {
                backoff_secs = 1; // reset on success
                info!("connected to gateway");
                stream
            }
            Err(e) => {
                error!(err = %e, backoff = backoff_secs, "gateway connection failed, retrying");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = shutdown_rx.changed() => { return Ok(()); }
                }
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let (ws_tx, mut ws_rx) = ws_stream.split();
        let ws_tx: SharedWsTx = Arc::new(Mutex::new(ws_tx));
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let adapter: Arc<dyn ChatAdapter> =
            Arc::new(GatewayAdapter::new(ws_tx.clone(), pending.clone(), platform, streaming));
        let slash_ws_tx = ws_tx.clone(); // for fire-and-forget slash command responses
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                    msg = ws_rx.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                let text_str: &str = &text;

                                // Check if it's a response to a pending command
                                if let Ok(resp) = serde_json::from_str::<GatewayResponse>(text_str) {
                                if resp.schema == "openab.gateway.response.v1" {
                                    if let Some(tx) = pending.lock().await.remove(&resp.request_id) {
                                        let _ = tx.send(resp);
                                    }
                                    continue;
                                }
                            }

                            match serde_json::from_str::<GatewayEvent>(text_str) {
                                Ok(event) => {
                                    // TODO: gateway adapters (feishu) do their own bot filtering
                                    // via AllowBots + trusted_bot_ids, but Telegram does not.
                                    // When Feishu lifts the bot-to-bot delivery restriction,
                                    // this guard needs to become adapter-aware (e.g. a field on
                                    // GatewayEvent indicating the adapter already filtered bots).
                                    if event.sender.is_bot {
                                        continue;
                                    }

                                    // Channel allowlist gate
                                    if !allow_all_channels && !allowed_channels.contains(&event.channel.id) {
                                        info!(channel = %event.channel.id, "gateway: channel not in allowed_channels, skipping");
                                        continue;
                                    }

                                    // User allowlist gate
                                    if !allow_all_users && !allowed_users.contains(&event.sender.id) {
                                        info!(sender = %event.sender.id, "gateway: user not in allowed_users, skipping");
                                        continue;
                                    }

                                    // @mention gating: in groups, only respond if bot is mentioned
                                    // DMs (private) and thread replies always pass through
                                    let is_group = event.channel.channel_type == "group"
                                        || event.channel.channel_type == "supergroup";
                                    let in_thread = event.channel.thread_id.is_some();
                                    if is_group && !in_thread {
                                        if let Some(ref bot_name) = bot_username {
                                            let mentioned = event.mentions.iter().any(|m| m == bot_name);
                                            if !mentioned {
                                                continue; // skip non-mentioned group messages
                                            }
                                        }
                                    }

                                    info!(
                                        platform = %event.platform,
                                        sender = %event.sender.name,
                                        channel = %event.channel.id,
                                        "gateway event received"
                                    );

                                    let channel = ChannelRef {
                                        platform: event.platform.clone(),
                                        channel_id: event.channel.id.clone(),
                                        thread_id: event.channel.thread_id.clone(),
                                        parent_id: None,
                                        origin_event_id: Some(event.event_id.clone()),
                                    };

                                    let sender_ctx = SenderContext {
                                        schema: "openab.sender.v1".into(),
                                        sender_id: event.sender.id.clone(),
                                        sender_name: event.sender.name.clone(),
                                        display_name: event.sender.display_name.clone(),
                                        channel: event.channel.channel_type.clone(),
                                        channel_id: event.channel.id.clone(),
                                        thread_id: event.channel.thread_id.clone(),
                                        is_bot: event.sender.is_bot,
                                    };
                                    let sender_json = serde_json::to_string(&sender_ctx)
                                        .unwrap_or_default();

                                    let trigger_msg = MessageRef {
                                        channel: channel.clone(),
                                        message_id: event.message_id.clone(),
                                    };

                                    let adapter = adapter.clone();
                                    let router = router.clone();
                                    let prompt = event.content.text.clone();

                                    // Slash command interception for gateway platforms
                                    // (Feishu/LINE/Telegram don't have native slash commands)
                                    // Use fire-and-forget send — slash command responses don't
                                    // need message_id for streaming edits.
                                    let trimmed = prompt.trim();
                                    if trimmed == "/reset" {
                                        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
                                        let msg = match router.pool().reset_session(&thread_key).await {
                                            Ok(()) => "🔄 Session reset. Start a new conversation!",
                                            Err(_) => "⚠️ No active session to reset.",
                                        };
                                        let _ = send_fire_and_forget(&slash_ws_tx, &channel, msg).await;
                                        continue;
                                    }
                                    if trimmed == "/cancel" {
                                        let thread_key = format!("{}:{}", event.platform, event.channel.thread_id.as_deref().unwrap_or(&event.channel.id));
                                        let msg = match router.pool().cancel_session(&thread_key).await {
                                            Ok(()) => "🛑 Cancel signal sent.".to_string(),
                                            Err(e) => format!("⚠️ {e}"),
                                        };
                                        let _ = send_fire_and_forget(&slash_ws_tx, &channel, &msg).await;
                                        continue;
                                    }

                                    tasks.spawn(async move {
                                        // If supergroup with no thread_id, create a forum topic
                                        let thread_channel = if event.channel.channel_type == "supergroup"
                                            && channel.thread_id.is_none()
                                        {
                                            let title = crate::format::shorten_thread_name(&prompt);
                                            match adapter.create_thread(&channel, &trigger_msg, &title).await {
                                                Ok(tc) => tc,
                                                Err(e) => {
                                                    warn!("create_thread failed, using channel: {e}");
                                                    channel.clone()
                                                }
                                            }
                                        } else {
                                            channel.clone()
                                        };

                                        if let Err(e) = router
                                            .handle_message(
                                                &adapter,
                                                &thread_channel,
                                                &sender_json,
                                                &prompt,
                                                vec![],
                                                &trigger_msg,
                                                false,
                                            )
                                            .await
                                        {
                                            error!("gateway message handling error: {e}");
                                        }
                                    });
                                }
                                Err(e) => warn!("invalid gateway event: {e}"),
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            warn!("gateway WebSocket closed, will reconnect");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("gateway WebSocket error: {e}, will reconnect");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("gateway adapter shutting down, waiting for {} in-flight tasks", tasks.len());
                        while tasks.join_next().await.is_some() {}
                        return Ok(());
                    }
                }
            }
        } // inner loop — break here means reconnect

        // Drain in-flight tasks before reconnecting
        while tasks.join_next().await.is_some() {}

        warn!(backoff = backoff_secs, "reconnecting to gateway");
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => { return Ok(()); }
        }
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF);
    } // outer reconnect loop
}

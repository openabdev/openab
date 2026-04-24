use crate::adapter::{AdapterRouter, ChatAdapter, ChannelRef, MessageRef, SenderContext};
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
    platform: String,
    channel: GwChannel,
    sender: GwSender,
    content: GwContent,
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
    error: Option<String>,
}

// --- GatewayAdapter: ChatAdapter over WebSocket ---

type PendingRequests = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<GatewayResponse>>>>;

pub struct GatewayAdapter {
    ws_tx: Mutex<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >>,
    pending: PendingRequests,
}

impl GatewayAdapter {
    fn new(
        ws_tx: futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            Message,
        >,
        pending: PendingRequests,
    ) -> Self {
        Self {
            ws_tx: Mutex::new(ws_tx),
            pending,
        }
    }
}

#[async_trait]
impl ChatAdapter for GatewayAdapter {
    fn platform(&self) -> &'static str {
        "gateway"
    }

    fn message_limit(&self) -> usize {
        4096 // Telegram limit
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let reply = GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: String::new(),
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
        self.ws_tx
            .lock()
            .await
            .send(Message::Text(json.into()))
            .await?;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: "gw_sent".into(),
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
        self.ws_tx.lock().await.send(Message::Text(json.into())).await?;

        // Wait for response (5s timeout)
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(resp)) if resp.success => {
                Ok(ChannelRef {
                    platform: channel.platform.clone(),
                    channel_id: channel.channel_id.clone(),
                    thread_id: resp.thread_id,
                    parent_id: None,
                })
            }
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

    async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(()) // no-op for PoC
    }

    async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(()) // no-op for PoC
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false // send-once for Telegram
    }
}

// --- Run the gateway adapter (connects to gateway WS, routes events to AdapterRouter) ---

pub async fn run_gateway_adapter(
    gateway_url: String,
    router: Arc<AdapterRouter>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    info!(url = %gateway_url, "connecting to custom gateway");

    let (ws_stream, _) = tokio_tungstenite::connect_async(&gateway_url).await?;
    info!("connected to gateway");

    let (ws_tx, mut ws_rx) = ws_stream.split();
    let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
    let adapter: Arc<dyn ChatAdapter> = Arc::new(GatewayAdapter::new(ws_tx, pending.clone()));

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
                                if event.sender.is_bot {
                                    continue; // skip bot messages
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
                                };

                                let sender_ctx = SenderContext {
                                    schema: "sender.v1".into(),
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

                                tokio::spawn(async move {
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
                        warn!("gateway WebSocket closed");
                        break;
                    }
                    Some(Err(e)) => {
                        error!("gateway WebSocket error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("gateway adapter shutting down");
                    break;
                }
            }
        }
    }
    Ok(())
}

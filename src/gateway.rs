use crate::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef, SenderContext};
use crate::acp::ContentBlock;
use crate::config::SttConfig;
use crate::media;
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
struct GwAttachment {
    url: String,
    content_type: Option<String>,
    filename: Option<String>,
    size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct GwContent {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
    #[serde(default)]
    attachments: Vec<GwAttachment>,
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
    ws_tx: Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
    pending: PendingRequests,
    platform_name: &'static str,
}

impl GatewayAdapter {
    fn new(
        ws_tx: futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        pending: PendingRequests,
        platform_name: &'static str,
    ) -> Self {
        Self {
            ws_tx: Mutex::new(ws_tx),
            pending,
            platform_name,
        }
    }
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
        self.ws_tx.lock().await.send(Message::Text(json)).await?;
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

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false // send-once for Telegram
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
    pub stt_config: SttConfig,
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
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let adapter: Arc<dyn ChatAdapter> =
            Arc::new(GatewayAdapter::new(ws_tx, pending.clone(), platform));
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
                                    if event.sender.is_bot {
                                        continue; // skip bot messages
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
                                    let prompt = event.content.text.clone().unwrap_or_default();
                                    let attachments = event.content.attachments.clone();
                                    let stt_config = params.stt_config.clone();

                                    tasks.spawn(async move {
                                        let mut extra_blocks = Vec::new();
                                        let mut text_file_bytes: u64 = 0;
                                        let mut text_file_count: u32 = 0;
                                        const TEXT_TOTAL_CAP: u64 = 1024 * 1024; // 1 MB total for all text file attachments
                                        const TEXT_FILE_COUNT_CAP: u32 = 5;

                                        for attachment in attachments {
                                            let mime = attachment.content_type.as_deref().unwrap_or("");
                                            let filename = attachment.filename.as_deref().unwrap_or("attachment");
                                            let size = attachment.size.unwrap_or(0);

                                            if media::is_audio_mime(mime) {
                                                if stt_config.enabled {
                                                    let mime_clean = mime.split(';').next().unwrap_or(mime).trim();
                                                    if let Some(transcript) = media::download_and_transcribe(
                                                        &attachment.url,
                                                        filename,
                                                        mime_clean,
                                                        size,
                                                        &stt_config,
                                                        None,
                                                    ).await {
                                                        tracing::debug!(filename = %filename, chars = transcript.len(), "voice transcript injected");
                                                        extra_blocks.insert(0, ContentBlock::Text {
                                                            text: format!("[Voice message transcript]: {transcript}"),
                                                        });
                                                    }
                                                } else {
                                                    tracing::warn!(filename = %filename, "skipping audio attachment (STT disabled)");
                                                }
                                            } else if media::is_text_file(filename, attachment.content_type.as_deref()) {
                                                if text_file_count >= TEXT_FILE_COUNT_CAP {
                                                    tracing::warn!(filename = %filename, count = text_file_count, "text file count cap reached, skipping");
                                                    continue;
                                                }
                                                if text_file_bytes + size > TEXT_TOTAL_CAP {
                                                    tracing::warn!(filename = %filename, total = text_file_bytes, "text attachments total exceeds 1MB cap, skipping remaining");
                                                    continue;
                                                }
                                                if let Some((block, actual_bytes)) = media::download_and_read_text_file(
                                                    &attachment.url,
                                                    filename,
                                                    size,
                                                    None,
                                                ).await {
                                                    text_file_bytes += actual_bytes;
                                                    text_file_count += 1;
                                                    tracing::debug!(filename = %filename, "adding text file attachment");
                                                    extra_blocks.push(block);
                                                }
                                            } else if let Some(block) = media::download_and_encode_image(
                                                &attachment.url,
                                                attachment.content_type.as_deref(),
                                                filename,
                                                size,
                                                None,
                                            ).await {
                                                tracing::debug!(url = %attachment.url, filename = %filename, "adding image attachment");
                                                extra_blocks.push(block);
                                            } else {
                                                tracing::debug!(url = %attachment.url, filename = %filename, mime = %mime, "ignoring unsupported attachment type");
                                                extra_blocks.push(ContentBlock::Text {
                                                    text: format!("[User uploaded a file: {filename} ({mime})]"),
                                                });
                                            }
                                        }

                                        if prompt.is_empty() && extra_blocks.is_empty() {
                                            return;
                                        }

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
                                                extra_blocks,
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

//! UnifiedGatewayAdapter — routes ChatAdapter calls through in-process gateway
//! platform adapters based on the ChannelRef.platform field.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use openab_core::adapter::{ChannelRef, ChatAdapter, MessageRef};
use openab_gateway::schema::{Content, GatewayReply, GatewayResponse, ReplyChannel};
use openab_gateway::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

/// How long `create_thread` waits for the platform `create_topic` response.
const CREATE_TOPIC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct UnifiedGatewayAdapter {
    pub gw_state: Arc<AppState>,
    /// Telegram reaction state (message_id -> emoji list) for add/remove_reaction
    pub telegram_reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl UnifiedGatewayAdapter {
    pub fn new(gw_state: Arc<AppState>) -> Self {
        Self {
            gw_state,
            telegram_reaction_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// If `json` is a gateway command response (`openab.gateway.response.v1`),
    /// deliver it to any waiter registered via [`Self::create_thread`] and
    /// return `true`. Returns `false` for ordinary inbound events so the
    /// caller can keep processing them as `GatewayEvent`s.
    ///
    /// Without this demux, `createForumTopic` responses are fed into
    /// `process_gateway_event`, which fails with
    /// `invalid type: null, expected a string` (response fields do not match
    /// the event schema) and the real `message_thread_id` is discarded.
    pub async fn try_complete_pending(&self, json: &str) -> bool {
        let Ok(resp) = serde_json::from_str::<GatewayResponse>(json) else {
            return false;
        };
        if resp.schema != "openab.gateway.response.v1" {
            return false;
        }
        if let Some(tx) = self
            .gw_state
            .pending_commands
            .lock()
            .await
            .remove(&resp.request_id)
        {
            let _ = tx.send(resp);
        }
        true
    }

    /// Dispatch a GatewayReply to the correct platform adapter.
    async fn dispatch_reply(&self, reply: &GatewayReply) {
        let client = &self.gw_state.client;
        match reply.platform.as_str() {
            #[cfg(feature = "telegram")]
            "telegram" => {
                if let Some(ref token) = self.gw_state.telegram_bot_token {
                    openab_gateway::adapters::telegram::handle_reply(
                        reply,
                        token,
                        client,
                        &self.gw_state.event_tx,
                        &self.telegram_reaction_state,
                        self.gw_state.telegram_rich_messages,
                    )
                    .await;
                }
            }
            #[cfg(feature = "line")]
            "line" => {
                if let Some(ref access_token) = self.gw_state.line_access_token {
                    openab_gateway::adapters::line::dispatch_line_reply(
                        client,
                        access_token,
                        &self.gw_state.reply_token_cache,
                        reply,
                        openab_gateway::adapters::line::LINE_API_BASE,
                    )
                    .await;
                }
            }
            #[cfg(feature = "feishu")]
            "feishu" => {
                if let Some(ref feishu) = self.gw_state.feishu {
                    openab_gateway::adapters::feishu::handle_reply(
                        reply,
                        feishu,
                        &self.gw_state.event_tx,
                    )
                    .await;
                }
            }
            #[cfg(feature = "googlechat")]
            "googlechat" => {
                if let Some(ref gc) = self.gw_state.google_chat {
                    gc.handle_reply(reply, &self.gw_state.event_tx).await;
                }
            }
            #[cfg(feature = "wecom")]
            "wecom" => {
                if let Some(ref wecom) = self.gw_state.wecom {
                    wecom.handle_reply(reply, &self.gw_state.event_tx).await;
                }
            }
            #[cfg(feature = "lineworks")]
            "lineworks" => {
                if let Some(ref lineworks) = self.gw_state.lineworks {
                    let ok = openab_gateway::adapters::lineworks::dispatch_lineworks_reply(
                        client, lineworks, reply,
                    )
                    .await;
                    if !ok {
                        tracing::error!(
                            channel = %reply.channel.id,
                            command = ?reply.command.as_deref(),
                            "lineworks reply delivery failed — reply lost"
                        );
                    }
                }
            }
            #[cfg(feature = "teams")]
            "teams" => {
                if let Some(ref teams) = self.gw_state.teams {
                    openab_gateway::adapters::teams::handle_reply(
                        reply,
                        teams,
                        &self.gw_state.teams_service_urls,
                    )
                    .await;
                }
            }
            #[cfg(feature = "acp")]
            "acp" => {
                if let Some(ref registry) = self.gw_state.acp_reply_registry {
                    openab_gateway::adapters::acp_server::handle_reply(reply, registry).await;
                }
            }
            other => {
                tracing::warn!(platform = other, "unified adapter: unknown platform, cannot route reply");
            }
        }
    }

    /// Build a GatewayReply from ChatAdapter parameters.
    fn build_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        command: Option<&str>,
        quote_message_id: Option<&str>,
    ) -> GatewayReply {
        GatewayReply {
            schema: "openab.gateway.reply.v1".into(),
            reply_to: channel.origin_event_id.clone().unwrap_or_default(),
            platform: channel.platform.clone(),
            channel: ReplyChannel {
                id: channel.channel_id.clone(),
                thread_id: channel.thread_id.clone(),
            },
            content: Content {
                content_type: "text".into(),
                text: content.into(),
                attachments: vec![],
            },
            command: command.map(|s| s.into()),
            request_id: None,
            quote_message_id: quote_message_id.map(|s| s.into()),
        }
    }
}

#[async_trait]
impl ChatAdapter for UnifiedGatewayAdapter {
    fn platform(&self) -> &'static str {
        "unified"
    }

    fn message_limit(&self) -> usize {
        4096 // conservative limit across platforms
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        let reply = self.build_reply(channel, content, None, None);
        self.dispatch_reply(&reply).await;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: format!("unified_{:x}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()),
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        // Mirror the standalone GatewayAdapter path: register a oneshot, send
        // create_topic with request_id, wait for GatewayResponse carrying the
        // real platform thread id (Telegram message_thread_id).
        //
        // The previous implementation ignored the response and returned
        // trigger_msg.message_id as thread_id. For Telegram forums that value
        // is a *message* id, not a topic id, so replies fail with
        // "Bad Request: message thread not found" after a topic was created.
        let req_id = format!("req_{}", Uuid::new_v4());
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.gw_state
            .pending_commands
            .lock()
            .await
            .insert(req_id.clone(), tx);

        let mut reply = self.build_reply(channel, title, Some("create_topic"), None);
        reply.request_id = Some(req_id.clone());
        self.dispatch_reply(&reply).await;

        match tokio::time::timeout(CREATE_TOPIC_TIMEOUT, rx).await {
            Ok(Ok(resp)) if resp.success => {
                if let Some(thread_id) = resp.thread_id {
                    Ok(ChannelRef {
                        platform: channel.platform.clone(),
                        channel_id: channel.channel_id.clone(),
                        thread_id: Some(thread_id),
                        parent_id: Some(channel.channel_id.clone()),
                        origin_event_id: channel.origin_event_id.clone(),
                    })
                } else {
                    warn!(
                        request_id = %req_id,
                        "create_topic succeeded but thread_id missing; falling back to parent channel"
                    );
                    Ok(channel.clone())
                }
            }
            Ok(Ok(resp)) => {
                warn!(
                    err = ?resp.error,
                    request_id = %req_id,
                    "create_topic failed, falling back to same channel"
                );
                Ok(channel.clone())
            }
            Ok(Err(_)) => {
                // oneshot dropped — treat as failure
                self.gw_state.pending_commands.lock().await.remove(&req_id);
                Err(anyhow!("create_topic response channel closed"))
            }
            Err(_) => {
                warn!(
                    request_id = %req_id,
                    "create_topic timeout, falling back to same channel"
                );
                self.gw_state.pending_commands.lock().await.remove(&req_id);
                Ok(channel.clone())
            }
        }
    }

    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, emoji, Some("add_reaction"), None);
        // Use the actual platform message_id (not origin_event_id which is a UUID)
        reply.reply_to = msg.message_id.clone();
        self.dispatch_reply(&reply).await;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, emoji, Some("remove_reaction"), None);
        // Use the actual platform message_id (not origin_event_id which is a UUID)
        reply.reply_to = msg.message_id.clone();
        self.dispatch_reply(&reply).await;
        Ok(())
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, content, Some("edit_message"), None);
        // Use the actual platform message_id (e.g. "draft" for streaming, or numeric for edits)
        reply.reply_to = msg.message_id.clone();
        self.dispatch_reply(&reply).await;
        Ok(())
    }

    async fn send_message_with_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        reply_to_message_id: &str,
    ) -> Result<MessageRef> {
        let reply = self.build_reply(channel, content, None, Some(reply_to_message_id));
        self.dispatch_reply(&reply).await;
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: format!("unified_{:x}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()),
        })
    }

    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        // Streaming override is resolved once at startup (config `[telegram].streaming`
        // → `TELEGRAM_STREAMING` env → unset). When unset, default to `true` when
        // Telegram Rich Messages are enabled (implies sendRichMessageDraft support),
        // `false` otherwise. This gives Telegram-only deployments streaming out of the
        // box while multi-platform deployments stay safe by default.
        if let Some(streaming) = self.gw_state.telegram_streaming {
            return streaming;
        }
        self.gw_state.telegram_rich_messages
    }

    fn show_streaming_placeholder(&self) -> bool {
        // No placeholder needed — Telegram uses sendRichMessageDraft for streaming preview.
        // The draft mechanism handles the "typing" indicator natively.
        false
    }

    fn renders_native_tables(&self, platform: &str) -> bool {
        // Telegram Rich Messages render markdown tables natively — skip the
        // table→code-block pre-pass so tables display with proper formatting.
        // Only applies to Telegram; other platforms in unified mode keep wrapping.
        platform == "telegram" && self.gw_state.telegram_rich_messages
    }
}

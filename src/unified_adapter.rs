//! UnifiedGatewayAdapter — routes ChatAdapter calls through in-process gateway
//! platform adapters based on the ChannelRef.platform field.

use anyhow::Result;
use async_trait::async_trait;
use openab_core::adapter::{
    AdapterCapabilities, ChannelRef, ChatAdapter, MessageLimit, MessageRef, StatusBackend,
    StreamingMode,
};
use openab_gateway::schema::{Content, GatewayReply, ReplyChannel};
use openab_gateway::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

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
        4096 // conservative legacy limit across platforms
    }

    fn capabilities(&self, platform: &str) -> AdapterCapabilities {
        let telegram_streaming = self
            .gw_state
            .telegram_streaming
            .unwrap_or(self.gw_state.telegram_rich_messages);
        let (can_edit, can_delete, streaming_mode, status_backend) = match platform {
            "telegram" => (
                self.gw_state.telegram_rich_messages,
                false,
                if telegram_streaming && self.gw_state.telegram_rich_messages {
                    StreamingMode::Edit
                } else {
                    StreamingMode::Disabled
                },
                StatusBackend::Reactions,
            ),
            // Unified mode currently has no per-platform streaming switch for
            // these adapters. Keep them send-once rather than inheriting the
            // unrelated Telegram setting.
            "feishu" => (
                true,
                true,
                StreamingMode::Disabled,
                StatusBackend::Reactions,
            ),
            "googlechat" => (
                true,
                false,
                StreamingMode::Disabled,
                StatusBackend::Reactions,
            ),
            "wecom" => (false, false, StreamingMode::Disabled, StatusBackend::None),
            "teams" | "line" | "lineworks" | "acp" => {
                (false, false, StreamingMode::Disabled, StatusBackend::None)
            }
            _ => (
                false,
                false,
                StreamingMode::Disabled,
                StatusBackend::Reactions,
            ),
        };
        AdapterCapabilities {
            send_ack: false,
            edit_ack: false,
            delete_ack: false,
            can_edit,
            can_delete,
            streaming_mode,
            show_streaming_placeholder: !(platform == "telegram"
                && self.gw_state.telegram_rich_messages),
            message_limit: match platform {
                "acp" => MessageLimit::Unlimited,
                "lineworks" => MessageLimit::Characters { max: 2000 },
                "wecom" => MessageLimit::Characters { max: 2048 },
                _ => MessageLimit::Characters { max: 4096 },
            },
            status_backend,
        }
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
        trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        let reply = self.build_reply(channel, title, Some("create_topic"), None);
        self.dispatch_reply(&reply).await;
        // Return a thread channel ref with the trigger message as thread_id
        Ok(ChannelRef {
            platform: channel.platform.clone(),
            channel_id: channel.channel_id.clone(),
            thread_id: Some(trigger_msg.message_id.clone()),
            parent_id: Some(channel.channel_id.clone()),
            origin_event_id: channel.origin_event_id.clone(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter_with_telegram_streaming(streaming: bool) -> UnifiedGatewayAdapter {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(4);
        let mut state = AppState::test_default(event_tx);
        state.telegram_streaming = Some(streaming);
        state.telegram_rich_messages = streaming;
        UnifiedGatewayAdapter::new(Arc::new(state))
    }

    #[test]
    fn teams_capabilities_do_not_inherit_telegram_streaming() {
        let adapter = adapter_with_telegram_streaming(true);
        let capabilities = adapter.capabilities("teams");
        assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        assert!(!capabilities.can_edit);
        assert_eq!(capabilities.status_backend, StatusBackend::None);
    }

    #[test]
    fn telegram_capabilities_follow_telegram_streaming() {
        let adapter = adapter_with_telegram_streaming(true);
        let capabilities = adapter.capabilities("telegram");
        assert_eq!(capabilities.streaming_mode, StreamingMode::Edit);
        assert!(!capabilities.show_streaming_placeholder);
    }

    #[test]
    fn telegram_without_rich_drafts_is_send_once() {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(4);
        let mut state = AppState::test_default(event_tx);
        state.telegram_streaming = Some(true);
        state.telegram_rich_messages = false;
        let adapter = UnifiedGatewayAdapter::new(Arc::new(state));

        let capabilities = adapter.capabilities("telegram");
        assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        assert!(!capabilities.can_edit);
    }
}

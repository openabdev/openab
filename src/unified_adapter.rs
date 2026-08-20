//! UnifiedGatewayAdapter — routes ChatAdapter calls through in-process gateway
//! platform adapters based on the ChannelRef.platform field.

use anyhow::Result;
use async_trait::async_trait;
use openab_core::adapter::{
    AdapterCapabilities, ChannelRef, ChatAdapter, MessageLimit, MessageRef, StatusBackend,
    StreamingMode,
};
#[cfg(feature = "teams")]
use openab_gateway::schema::WriteOutcome;
use openab_gateway::schema::{Content, GatewayReply, ReplyChannel};
use openab_gateway::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct UnifiedGatewayAdapter {
    pub gw_state: Arc<AppState>,
    /// Telegram reaction state (message_id -> emoji list) for add/remove_reaction
    pub telegram_reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Core-side opt-in. Teams processing messages reuse the existing real-ID
    /// send/edit/delete primitives and remain independent from reaction preview.
    teams_processing_indicator: bool,
}

impl UnifiedGatewayAdapter {
    pub fn new(gw_state: Arc<AppState>) -> Self {
        Self {
            gw_state,
            telegram_reaction_state: Arc::new(Mutex::new(HashMap::new())),
            teams_processing_indicator: false,
        }
    }

    pub fn with_teams_processing_indicator(mut self, enabled: bool) -> Self {
        self.teams_processing_indicator = enabled;
        self
    }

    /// Dispatch a GatewayReply to the correct platform adapter.
    async fn dispatch_reply(&self, reply: &GatewayReply) -> Result<Option<String>> {
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
                let teams = self
                    .gw_state
                    .teams
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Teams adapter is not configured"))?;
                let outcome = openab_gateway::adapters::teams::handle_reply(reply, teams).await;
                return Self::teams_outcome_result(outcome, reply.command.is_none());
            }
            #[cfg(feature = "acp")]
            "acp" => {
                if let Some(ref registry) = self.gw_state.acp_reply_registry {
                    openab_gateway::adapters::acp_server::handle_reply(reply, registry).await;
                }
            }
            other => {
                tracing::warn!(
                    platform = other,
                    "unified adapter: unknown platform, cannot route reply"
                );
            }
        }
        Ok(None)
    }

    #[cfg(feature = "teams")]
    fn teams_outcome_result(
        outcome: WriteOutcome,
        require_message_id: bool,
    ) -> Result<Option<String>> {
        match outcome {
            WriteOutcome::Delivered {
                message_id: Some(message_id),
            } => Ok(Some(message_id)),
            WriteOutcome::Delivered { message_id: None } if require_message_id => Err(
                anyhow::anyhow!("Teams delivered send without an activity id"),
            ),
            WriteOutcome::Delivered { message_id: None } => Ok(None),
            WriteOutcome::Rejected { code, message, .. } => {
                Err(anyhow::anyhow!("Teams rejected write ({code}): {message}"))
            }
            WriteOutcome::Unknown { code, message } => Err(anyhow::anyhow!(
                "Teams write outcome unknown ({code}): {message}"
            )),
        }
    }

    fn synthetic_message_id() -> String {
        format!(
            "unified_{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
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
            target_message_id: None,
        }
    }

    fn apply_command_target(&self, reply: &mut GatewayReply, msg: &MessageRef) {
        if self
            .capabilities(&msg.channel.platform)
            .supports_target_message_id
        {
            reply.target_message_id = Some(msg.message_id.clone());
        } else {
            reply.reply_to = msg.message_id.clone();
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
        #[cfg(feature = "teams")]
        let teams_reactions = self
            .gw_state
            .teams
            .as_ref()
            .is_some_and(|teams| teams.reactions_enabled());
        #[cfg(not(feature = "teams"))]
        let teams_reactions = false;
        let (can_edit, can_delete, streaming_mode, supports_reactions, status_backend) =
            match platform {
                "telegram" => (
                    self.gw_state.telegram_rich_messages,
                    false,
                    if telegram_streaming && self.gw_state.telegram_rich_messages {
                        StreamingMode::Edit
                    } else {
                        StreamingMode::Disabled
                    },
                    true,
                    StatusBackend::Reactions,
                ),
            // Unified mode currently has no per-platform streaming switch for
            // these adapters. Keep them send-once rather than inheriting the
            // unrelated Telegram setting.
                "feishu" => (
                    true,
                    true,
                    StreamingMode::Disabled,
                    true,
                    StatusBackend::Reactions,
                ),
                "googlechat" => (
                    true,
                    false,
                    StreamingMode::Disabled,
                    true,
                    StatusBackend::Reactions,
                ),
                "wecom" => (
                    false,
                    false,
                    StreamingMode::Disabled,
                    false,
                    StatusBackend::None,
                ),
                "teams" => (
                    cfg!(feature = "teams"),
                    cfg!(feature = "teams"),
                    StreamingMode::Disabled,
                    teams_reactions,
                    if self.teams_processing_indicator && self.gw_state.teams.is_some() {
                        StatusBackend::Message
                    } else if teams_reactions {
                        StatusBackend::Reactions
                    } else {
                        StatusBackend::None
                    },
                ),
                "line" | "lineworks" | "acp" => (
                    false,
                    false,
                    StreamingMode::Disabled,
                    false,
                    StatusBackend::None,
                ),
                _ => (
                    false,
                    false,
                    StreamingMode::Disabled,
                    true,
                    StatusBackend::Reactions,
                ),
            };
        AdapterCapabilities {
            send_ack: cfg!(feature = "teams") && platform == "teams",
            edit_ack: cfg!(feature = "teams") && platform == "teams",
            delete_ack: cfg!(feature = "teams") && platform == "teams",
            supports_target_message_id: cfg!(feature = "teams") && platform == "teams",
            can_edit,
            can_delete,
            streaming_mode,
            supports_reactions,
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
        let message_id = self
            .dispatch_reply(&reply)
            .await?
            .unwrap_or_else(Self::synthetic_message_id);
        Ok(MessageRef {
            channel: channel.clone(),
            message_id,
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        trigger_msg: &MessageRef,
        title: &str,
    ) -> Result<ChannelRef> {
        let reply = self.build_reply(channel, title, Some("create_topic"), None);
        self.dispatch_reply(&reply).await?;
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
        self.apply_command_target(&mut reply, msg);
        self.dispatch_reply(&reply).await?;
        Ok(())
    }

    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, emoji, Some("remove_reaction"), None);
        self.apply_command_target(&mut reply, msg);
        self.dispatch_reply(&reply).await?;
        Ok(())
    }

    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, content, Some("edit_message"), None);
        self.apply_command_target(&mut reply, msg);
        self.dispatch_reply(&reply).await?;
        Ok(())
    }

    async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
        let mut reply = self.build_reply(&msg.channel, "", Some("delete_message"), None);
        self.apply_command_target(&mut reply, msg);
        self.dispatch_reply(&reply).await?;
        Ok(())
    }

    async fn send_message_with_reply(
        &self,
        channel: &ChannelRef,
        content: &str,
        reply_to_message_id: &str,
    ) -> Result<MessageRef> {
        let reply = self.build_reply(channel, content, None, Some(reply_to_message_id));
        let message_id = self
            .dispatch_reply(&reply)
            .await?
            .unwrap_or_else(Self::synthetic_message_id);
        Ok(MessageRef {
            channel: channel.clone(),
            message_id,
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

    #[cfg(feature = "teams")]
    #[test]
    fn teams_capabilities_do_not_inherit_telegram_streaming() {
        let adapter = adapter_with_telegram_streaming(true);
        let capabilities = adapter.capabilities("teams");
        assert!(capabilities.send_ack);
        assert!(capabilities.edit_ack);
        assert!(capabilities.delete_ack);
        assert!(capabilities.supports_target_message_id);
        assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        assert!(capabilities.can_edit);
        assert!(capabilities.can_delete);
        assert!(!capabilities.supports_reactions);
        assert_eq!(capabilities.status_backend, StatusBackend::None);

        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(4);
        let mut state = AppState::test_default(event_tx);
        state.apply_teams_config(openab_gateway::GatewayTeamsConfig {
            app_id: Some("app".into()),
            app_secret: Some("secret".into()),
            allowed_tenants: Vec::new(),
            oauth_endpoint: "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token"
                .into(),
            openid_metadata: "https://login.botframework.com/v1/.well-known/openidconfiguration"
                .into(),
            webhook_path: "/webhook/teams".into(),
            dedupe_ttl_secs: 600,
            route_ttl_secs: 3600,
            max_route_entries: 10_000,
            reactions_enabled: true,
        });
        let adapter = UnifiedGatewayAdapter::new(Arc::new(state));
        let reaction_capabilities = adapter.capabilities("teams");
        assert!(reaction_capabilities.supports_reactions);
        assert_eq!(
            reaction_capabilities.status_backend,
            StatusBackend::Reactions
        );

        let message_adapter = adapter.with_teams_processing_indicator(true);
        let message_capabilities = message_adapter.capabilities("teams");
        assert!(message_capabilities.supports_reactions);
        assert_eq!(
            message_capabilities.status_backend,
            StatusBackend::Message
        );
    }

    #[cfg(feature = "teams")]
    #[test]
    fn teams_outcomes_return_real_activity_id_and_propagate_failure() -> Result<()> {
        assert_eq!(
            UnifiedGatewayAdapter::teams_outcome_result(
                WriteOutcome::Delivered {
                    message_id: Some("activity-1".into())
                },
                true,
            )?,
            Some("activity-1".into())
        );
        assert!(UnifiedGatewayAdapter::teams_outcome_result(
            WriteOutcome::Delivered { message_id: None },
            true,
        )
        .is_err());
        assert_eq!(
            UnifiedGatewayAdapter::teams_outcome_result(
                WriteOutcome::Delivered { message_id: None },
                false,
            )?,
            None
        );
        assert!(UnifiedGatewayAdapter::teams_outcome_result(
            WriteOutcome::Rejected {
                code: "route_not_found".into(),
                message: "missing".into(),
                retry_after_ms: None,
            },
            true,
        )
        .is_err());
        assert!(UnifiedGatewayAdapter::teams_outcome_result(
            WriteOutcome::Unknown {
                code: "request_timeout".into(),
                message: "ambiguous".into(),
            },
            true,
        )
        .is_err());
        Ok(())
    }

    #[cfg(feature = "teams")]
    #[test]
    fn teams_command_target_preserves_origin_event() {
        let adapter = adapter_with_telegram_streaming(false);
        let message = MessageRef {
            channel: ChannelRef {
                platform: "teams".into(),
                channel_id: "conversation-1".into(),
                thread_id: None,
                parent_id: None,
                origin_event_id: Some("event-1".into()),
            },
            message_id: "activity-1".into(),
        };
        let mut reply =
            adapter.build_reply(&message.channel, "updated", Some("edit_message"), None);
        adapter.apply_command_target(&mut reply, &message);
        assert_eq!(reply.reply_to, "event-1");
        assert_eq!(reply.target_message_id.as_deref(), Some("activity-1"));

        let mut legacy_message = message;
        legacy_message.channel.platform = "line".into();
        let mut legacy_reply = adapter.build_reply(
            &legacy_message.channel,
            "updated",
            Some("edit_message"),
            None,
        );
        adapter.apply_command_target(&mut legacy_reply, &legacy_message);
        assert_eq!(legacy_reply.reply_to, "activity-1");
        assert!(legacy_reply.target_message_id.is_none());
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

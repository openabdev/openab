//! UnifiedGatewayAdapter — routes ChatAdapter calls through in-process gateway
//! platform adapters based on the ChannelRef.platform field.

use anyhow::Result;
use async_trait::async_trait;
use openab_core::adapter::{
    AdapterCapabilities, ChannelRef, ChatAdapter, MaterializedAttachment, MessageLimit, MessageRef,
    StatusBackend, StreamingMode,
};
#[cfg(feature = "teams")]
use openab_core::adapter::{WriteFailure, WriteOutcome as CoreWriteOutcome};
use openab_core::gateway::apply_teams_progressive_capabilities;
#[cfg(feature = "teams")]
use openab_gateway::schema::WriteOutcome;
use openab_gateway::schema::{Content, GatewayReply, ReplyChannel, TEAMS_TEXT_UTF16_BUDGET_BYTES};
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
    /// Core-side default-off Teams progressive-content policy.
    teams_streaming: bool,
    /// Core-side default-off Teams inbound attachment policy.
    teams_inbound_attachments: bool,
}

impl UnifiedGatewayAdapter {
    pub fn new(gw_state: Arc<AppState>) -> Self {
        Self {
            gw_state,
            telegram_reaction_state: Arc::new(Mutex::new(HashMap::new())),
            teams_processing_indicator: false,
            teams_streaming: false,
            teams_inbound_attachments: false,
        }
    }

    pub fn with_teams_processing_indicator(mut self, enabled: bool) -> Self {
        self.teams_processing_indicator = enabled;
        self
    }

    pub fn with_teams_streaming(mut self, enabled: bool) -> Self {
        self.teams_streaming = enabled;
        self
    }

    pub fn with_teams_inbound_attachments(mut self, enabled: bool) -> Self {
        self.teams_inbound_attachments = enabled;
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
            WriteOutcome::Delivered { message_id: None } if require_message_id => {
                Err(WriteFailure::new(CoreWriteOutcome::Unknown {
                    code: "missing_message_id".into(),
                    message: "Teams delivered send without an activity id".into(),
                })
                .into())
            }
            WriteOutcome::Delivered { message_id: None } => Ok(None),
            WriteOutcome::Rejected {
                code,
                message,
                retry_after_ms,
            } => Err(WriteFailure::new(CoreWriteOutcome::Rejected {
                code,
                message,
                retry_after_ms,
            })
            .into()),
            WriteOutcome::Unknown { code, message } => {
                Err(WriteFailure::new(CoreWriteOutcome::Unknown { code, message }).into())
            }
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
            attachment_ref: None,
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
        let teams_available = self.gw_state.teams.is_some();
        #[cfg(not(feature = "teams"))]
        let teams_available = false;
        #[cfg(feature = "teams")]
        let teams_reactions = self
            .gw_state
            .teams
            .as_ref()
            .is_some_and(|teams| teams.reactions_enabled());
        #[cfg(not(feature = "teams"))]
        let teams_reactions = false;
        #[cfg(feature = "teams")]
        let teams_materialization = self
            .gw_state
            .teams
            .as_ref()
            .is_some_and(|teams| teams.inbound_attachments_enabled());
        #[cfg(not(feature = "teams"))]
        let teams_materialization = false;
        #[cfg(feature = "teams")]
        let teams_conversation_registry = self
            .gw_state
            .teams
            .as_ref()
            .is_some_and(|teams| teams.conversation_registry_available());
        #[cfg(not(feature = "teams"))]
        let teams_conversation_registry = false;
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
                    if self.teams_processing_indicator && teams_available {
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
        let mut capabilities = AdapterCapabilities {
            send_ack: cfg!(feature = "teams") && platform == "teams",
            edit_ack: cfg!(feature = "teams") && platform == "teams",
            delete_ack: cfg!(feature = "teams") && platform == "teams",
            supports_target_message_id: cfg!(feature = "teams") && platform == "teams",
            supports_attachment_materialization: platform == "teams"
                && teams_available
                && teams_materialization
                && self.teams_inbound_attachments,
            supports_conversation_registry: platform == "teams"
                && teams_available
                && teams_conversation_registry,
            can_edit,
            can_delete,
            streaming_mode,
            supports_reactions,
            show_streaming_placeholder: !(platform == "telegram"
                && self.gw_state.telegram_rich_messages),
            message_limit: match platform {
                "acp" => MessageLimit::Unlimited,
                "teams" if teams_available => MessageLimit::Utf16Bytes {
                    max: TEAMS_TEXT_UTF16_BUDGET_BYTES,
                },
                "lineworks" => MessageLimit::Characters { max: 2000 },
                "wecom" => MessageLimit::Characters { max: 2048 },
                _ => MessageLimit::Characters { max: 4096 },
            },
            status_backend,
        };
        if platform == "teams" {
            apply_teams_progressive_capabilities(
                teams_available,
                self.teams_streaming,
                &mut capabilities,
            );
        }
        capabilities
    }

    async fn materialize_attachment(
        &self,
        channel: &ChannelRef,
        reference: &str,
    ) -> Result<MaterializedAttachment> {
        if !self
            .capabilities(&channel.platform)
            .supports_attachment_materialization
        {
            anyhow::bail!("attachment materialization is unavailable");
        }
        #[cfg(feature = "teams")]
        {
            let event_id = channel
                .origin_event_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("attachment route is unavailable"))?;
            let teams = self
                .gw_state
                .teams
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Teams adapter is not configured"))?;
            let attachment = teams
                .materialize_attachment(event_id, &channel.channel_id, reference)
                .await?;
            if attachment.path.is_some() || attachment.reference.is_some() {
                anyhow::bail!("materialized Teams attachment returned an invalid envelope");
            }
            if !matches!(attachment.attachment_type.as_str(), "image" | "text_file")
                || attachment.filename.chars().count() > 200
                || attachment.filename.chars().any(char::is_control)
                || attachment.mime_type.len() > 128
                || attachment.mime_type.chars().any(char::is_control)
                || attachment.status.as_ref().is_some_and(|status| {
                    status.len() > 256 || status.chars().any(char::is_control)
                })
            {
                anyhow::bail!("materialized Teams attachment returned invalid metadata");
            }
            let data = attachment
                .decoded_data()
                .map_err(|_| anyhow::anyhow!("materialized attachment data is malformed"))?;
            if attachment.status.is_some() {
                if !data.is_empty() {
                    anyhow::bail!("rejected Teams attachment returned payload data");
                }
            } else if attachment.size != data.len() as u64 {
                anyhow::bail!("materialized Teams attachment size does not match its payload");
            }
            return Ok(MaterializedAttachment {
                attachment_type: attachment.attachment_type,
                filename: attachment.filename,
                mime_type: attachment.mime_type,
                data,
                size: attachment.size,
                status: attachment.status,
            });
        }
        #[cfg(not(feature = "teams"))]
        {
            let _ = (channel, reference);
            anyhow::bail!("Teams attachment materialization is not compiled")
        }
    }

    async fn register_conversation(&self, channel: &ChannelRef) -> Result<()> {
        if !self
            .capabilities(&channel.platform)
            .supports_conversation_registry
        {
            anyhow::bail!("conversation registry is unavailable");
        }
        if channel
            .origin_event_id
            .as_deref()
            .is_none_or(|event_id| event_id.trim().is_empty())
        {
            anyhow::bail!("conversation registration requires an origin event");
        }
        let reply = self.build_reply(channel, "", Some("register_conversation"), None);
        self.dispatch_reply(&reply).await?;
        Ok(())
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
        assert!(!capabilities.supports_attachment_materialization);
        assert!(!capabilities.supports_conversation_registry);
        assert_eq!(capabilities.streaming_mode, StreamingMode::Disabled);
        assert_eq!(
            adapter
                .with_teams_streaming(true)
                .capabilities("teams")
                .streaming_mode,
            StreamingMode::Disabled,
            "an opt-in without an embedded Teams adapter must fail closed"
        );
        assert!(capabilities.can_edit);
        assert!(capabilities.can_delete);
        assert!(!capabilities.supports_reactions);
        assert_eq!(capabilities.status_backend, StatusBackend::None);
        assert_eq!(
            capabilities.message_limit,
            MessageLimit::Characters { max: 4096 },
            "an unavailable embedded Teams adapter keeps the conservative fallback"
        );

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
            inbound_attachments: true,
            conversation_registry_path: None,
            conversation_registry_max_entries: 1_000,
            conversation_registry_ttl_secs: 365 * 24 * 60 * 60,
        });
        let state = Arc::new(state);
        let adapter = UnifiedGatewayAdapter::new(state.clone());
        let reaction_capabilities = adapter.capabilities("teams");
        assert!(reaction_capabilities.supports_reactions);
        assert!(!reaction_capabilities.supports_attachment_materialization);
        assert!(!reaction_capabilities.supports_conversation_registry);
        assert_eq!(
            reaction_capabilities.status_backend,
            StatusBackend::Reactions
        );
        assert_eq!(
            reaction_capabilities.message_limit,
            MessageLimit::Utf16Bytes {
                max: TEAMS_TEXT_UTF16_BUDGET_BYTES,
            }
        );

        let message_adapter = UnifiedGatewayAdapter::new(state)
            .with_teams_processing_indicator(true)
            .with_teams_streaming(true)
            .with_teams_inbound_attachments(true);
        let message_capabilities = message_adapter.capabilities("teams");
        assert!(message_capabilities.supports_attachment_materialization);
        assert!(message_capabilities.supports_reactions);
        assert_eq!(message_capabilities.status_backend, StatusBackend::Message);
        assert_eq!(message_capabilities.streaming_mode, StreamingMode::Edit);
        assert!(message_capabilities.show_streaming_placeholder);
    }

    #[cfg(feature = "teams")]
    #[tokio::test]
    async fn teams_registry_capability_and_command_are_gateway_local() -> Result<()> {
        let root = std::fs::canonicalize(std::env::temp_dir())?;
        let directory = root.join(format!(
            "openab-unified-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir(&directory)?;
        let registry_path = directory.join("registry.json");
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
            reactions_enabled: false,
            inbound_attachments: false,
            conversation_registry_path: Some(registry_path.to_string_lossy().into_owned()),
            conversation_registry_max_entries: 1_000,
            conversation_registry_ttl_secs: 365 * 24 * 60 * 60,
        });
        let adapter = UnifiedGatewayAdapter::new(Arc::new(state));
        assert!(adapter.capabilities("teams").supports_conversation_registry);
        let channel = ChannelRef {
            platform: "teams".into(),
            channel_id: "conversation-1".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("event-1".into()),
        };
        let reply = adapter.build_reply(&channel, "", Some("register_conversation"), None);
        let wire = serde_json::to_string(&reply)?;
        assert!(!wire.contains("service_url"));
        assert!(!wire.contains("serviceUrl"));
        assert!(adapter.register_conversation(&channel).await.is_err());
        assert!(!registry_path.exists());
        std::fs::remove_dir_all(directory)?;
        Ok(())
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
        let rejected = UnifiedGatewayAdapter::teams_outcome_result(
            WriteOutcome::Rejected {
                code: "route_not_found".into(),
                message: "missing".into(),
                retry_after_ms: None,
            },
            true,
        )
        .unwrap_err();
        assert!(matches!(
            &rejected.downcast_ref::<WriteFailure>().unwrap().outcome,
            CoreWriteOutcome::Rejected { code, .. } if code == "route_not_found"
        ));

        let unknown = UnifiedGatewayAdapter::teams_outcome_result(
            WriteOutcome::Unknown {
                code: "request_timeout".into(),
                message: "ambiguous".into(),
            },
            true,
        )
        .unwrap_err();
        assert!(matches!(
            &unknown.downcast_ref::<WriteFailure>().unwrap().outcome,
            CoreWriteOutcome::Unknown { code, .. } if code == "request_timeout"
        ));
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

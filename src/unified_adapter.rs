//! UnifiedGatewayAdapter — routes ChatAdapter calls through in-process gateway
//! platform adapters based on the ChannelRef.platform field.

use anyhow::Result;
use async_trait::async_trait;
use openab_core::adapter::{ChannelRef, ChatAdapter, MessageRef, StreamingStrategy};
use openab_gateway::schema::{Content, GatewayReply, GatewayResponse, ReplyChannel};
use openab_gateway::AppState;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

const GATEWAY_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    format!(
        "unified_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .saturating_mul(1_000_000)
            .saturating_add(REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed) as u128)
    )
}

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

    fn uses_feishu_card_streaming(&self, channel: &ChannelRef) -> bool {
        if channel.platform != "feishu" {
            return false;
        }

        #[cfg(feature = "feishu")]
        {
            self.gw_state.feishu.as_ref().is_some_and(|feishu| {
                feishu.config.streaming_mode
                    == openab_gateway::adapters::feishu::StreamingMode::Card
            })
        }

        #[cfg(not(feature = "feishu"))]
        false
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
                tracing::warn!(
                    platform = other,
                    "unified adapter: unknown platform, cannot route reply"
                );
            }
        }
    }

    /// Dispatch a reply and, when requested, wait for the platform response.
    /// Feishu returns the native om_... message ID; unified mode must preserve
    /// that ID because synthetic IDs cannot be edited by the Feishu API.
    async fn dispatch_with_response(
        &self,
        reply: &GatewayReply,
    ) -> Result<Option<GatewayResponse>> {
        let Some(request_id) = reply.request_id.as_deref() else {
            self.dispatch_reply(reply).await;
            return Ok(None);
        };
        if reply.platform != "feishu" {
            self.dispatch_reply(reply).await;
            return Ok(None);
        }

        let mut rx = self.gw_state.event_tx.subscribe();
        self.dispatch_reply(reply).await;
        let request_id = request_id.to_string();
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(json) => {
                        let Ok(response) = serde_json::from_str::<GatewayResponse>(&json) else {
                            continue;
                        };
                        if response.schema == "openab.gateway.response.v1"
                            && response.request_id == request_id
                        {
                            return Ok(response);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        anyhow::bail!("gateway response channel closed")
                    }
                }
            }
        };
        match tokio::time::timeout(GATEWAY_RESPONSE_TIMEOUT, wait).await {
            Ok(Ok(response)) if response.success => Ok(Some(response)),
            Ok(Ok(response)) => Err(anyhow::anyhow!(response
                .error
                .unwrap_or_else(|| "gateway reported failure".into()))),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow::anyhow!(
                "timed out waiting for gateway response to {request_id}"
            )),
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
            message_id: next_request_id(),
        })
    }

    async fn send_streaming_placeholder(
        &self,
        channel: &ChannelRef,
        content: &str,
    ) -> Result<MessageRef> {
        if !self.uses_feishu_card_streaming(channel) {
            return self.send_message(channel, content).await;
        }

        let mut reply = self.build_reply(channel, content, None, None);
        reply.request_id = Some(next_request_id());
        let message_id = self
            .dispatch_with_response(&reply)
            .await?
            .and_then(|response| response.message_id)
            .ok_or_else(|| anyhow::anyhow!("Feishu placeholder response omitted message_id"))?;
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
        if self.uses_feishu_card_streaming(&msg.channel) {
            reply.request_id = Some(next_request_id());
            self.dispatch_with_response(&reply).await?;
        } else {
            self.dispatch_reply(&reply).await;
        }
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
            message_id: next_request_id(),
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

    fn streaming_strategy(
        &self,
        channel: &ChannelRef,
        other_bot_present: bool,
    ) -> StreamingStrategy {
        if other_bot_present {
            return StreamingStrategy::Disabled;
        }

        match channel.platform.as_str() {
            "telegram" if self.use_streaming(false) => StreamingStrategy::Draft,
            "telegram" => StreamingStrategy::Disabled,
            #[cfg(feature = "feishu")]
            "feishu" => {
                match self
                    .gw_state
                    .feishu
                    .as_ref()
                    .map(|feishu| feishu.config.streaming_mode)
                {
                    // Card mode is an explicit operator opt-in. Keep the
                    // existing unified send-once behavior for post/auto so the
                    // bug fix does not change backward-compatible defaults.
                    Some(openab_gateway::adapters::feishu::StreamingMode::Card) => {
                        StreamingStrategy::EditablePlaceholder
                    }
                    _ => StreamingStrategy::Disabled,
                }
            }
            _ if !self.use_streaming(false) => StreamingStrategy::Disabled,
            _ => {
                if self.show_streaming_placeholder() {
                    StreamingStrategy::EditablePlaceholder
                } else {
                    StreamingStrategy::Draft
                }
            }
        }
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

    fn channel(platform: &str) -> ChannelRef {
        ChannelRef {
            platform: platform.into(),
            channel_id: "channel".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("event".into()),
        }
    }

    #[test]
    fn telegram_keeps_draft_strategy() {
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let mut state = AppState::test_default(event_tx);
        state.telegram_rich_messages = true;
        let adapter = UnifiedGatewayAdapter::new(Arc::new(state));

        assert_eq!(
            adapter.streaming_strategy(&channel("telegram"), false),
            StreamingStrategy::Draft,
        );
        assert_eq!(
            adapter.streaming_strategy(&channel("telegram"), true),
            StreamingStrategy::Disabled,
        );
    }

    #[cfg(feature = "feishu")]
    #[test]
    fn feishu_uses_real_editable_placeholder() {
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let mut state = AppState::test_default(event_tx);
        let mut pairs = std::collections::HashMap::new();
        pairs.insert("FEISHU_APP_ID".into(), "app".into());
        pairs.insert("FEISHU_APP_SECRET".into(), "secret".into());
        pairs.insert("FEISHU_CARD_STREAMING_MODE".into(), "card".into());
        state.apply_feishu_config(openab_gateway::GatewayFeishuConfig { pairs });
        let adapter = UnifiedGatewayAdapter::new(Arc::new(state));

        assert_eq!(
            adapter.streaming_strategy(&channel("feishu"), false),
            StreamingStrategy::EditablePlaceholder,
        );
        assert_eq!(
            adapter.streaming_strategy(&channel("feishu"), true),
            StreamingStrategy::Disabled,
        );
    }

    #[cfg(feature = "feishu")]
    #[tokio::test]
    async fn feishu_response_correlation_preserves_native_message_id() {
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(AppState::test_default(event_tx.clone()));
        let adapter = UnifiedGatewayAdapter::new(state);
        let mut reply = adapter.build_reply(&channel("feishu"), "…", None, None);
        reply.request_id = Some("request-1".into());

        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let response = GatewayResponse {
                schema: "openab.gateway.response.v1".into(),
                request_id: "request-1".into(),
                success: true,
                thread_id: None,
                message_id: Some("om_native123".into()),
                error: None,
            };
            event_tx
                .send(serde_json::to_string(&response).unwrap())
                .unwrap();
        });

        let response = adapter
            .dispatch_with_response(&reply)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.message_id.as_deref(), Some("om_native123"));
    }

    #[cfg(feature = "feishu")]
    #[tokio::test]
    async fn feishu_send_once_remains_fire_and_forget() {
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(AppState::test_default(event_tx));
        let adapter = UnifiedGatewayAdapter::new(state);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            adapter.send_message(&channel("feishu"), "complete response"),
        )
        .await
        .expect("send-once delivery must not wait for a gateway response")
        .unwrap();

        assert!(result.message_id.starts_with("unified_"));
    }
}

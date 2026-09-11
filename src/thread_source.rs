//! Thread capability source for the OAB MCP Facade.
//!
//! Exposes a `create_thread` tool that agents can call via `execute_capability`
//! to create a thread in a target channel. The adapter uses its own authenticated
//! HTTP client, so the thread owner is the bot itself — no token is exposed to
//! the agent subprocess.
//!
//! This closes the identity-safety gap where agents would otherwise call the
//! Discord REST API directly (via `exec` + `requests`), risking use of the wrong
//! bot's token or leaking credentials into logs and LLM context.

use std::sync::Arc;

use anyhow::Result;
use openab_core::adapter::{ChannelRef, ChatAdapter};
use openab_mcp::mcp::sources::{CapabilitySource, SessionCtx};
use openab_mcp::rmcp::model::Tool;
use serde_json::{json, Map, Value};

/// Facade capability source exposing platform thread-creation to agents.
///
/// Registered when a chat adapter is available, so the agent can open threads
/// in channels it is authorized for without handling credentials itself.
pub struct ThreadSource {
    adapter: Arc<dyn ChatAdapter>,
}

impl ThreadSource {
    pub fn new(adapter: Arc<dyn ChatAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait::async_trait]
impl CapabilitySource for ThreadSource {
    fn provider(&self) -> &str {
        "openab"
    }

    fn tools(&self, _ctx: Option<&SessionCtx>) -> Vec<Tool> {
        let schema = json!({
            "type": "object",
            "properties": {
                "channel_id": {
                    "type": "string",
                    "description": "The parent channel ID to create the thread in."
                },
                "name": {
                    "type": "string",
                    "description": "The thread title (max 100 characters)."
                },
                "content": {
                    "type": "string",
                    "description": "Optional starter message content for the thread."
                }
            },
            "required": ["channel_id", "name"]
        });
        vec![Tool::new(
            "create_thread",
            "Create a thread in a channel. The thread owner is the bot itself — no token is exposed to the caller. Returns the new thread's channel ID.",
            Arc::new(schema.as_object().unwrap().clone()),
        )]
    }

    async fn call(
        &self,
        _ctx: Option<&SessionCtx>,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Result<(Value, bool)> {
        if tool != "create_thread" {
            return Ok((json!({"error": format!("unknown tool: {tool}")}), true));
        }

        let channel_id = args
            .get("channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?;
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("name is required"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let channel = ChannelRef {
            platform: self.adapter.platform().into(),
            channel_id: channel_id.into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };

        match self
            .adapter
            .create_thread_in_channel(&channel, name, content)
            .await
        {
            Ok(thread_ref) => Ok((
                json!({
                    "thread_id": thread_ref.channel_id,
                    "parent_id": thread_ref.parent_id,
                    "platform": thread_ref.platform,
                }),
                false,
            )),
            Err(e) => Ok((json!({"error": format!("{e:#}")}), true)),
        }
    }

    fn requires_session(&self) -> bool {
        false
    }
}

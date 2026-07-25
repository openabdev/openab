//! Shared LLM type layer: message/content/tool/event types plus the
//! `LlmProvider` trait and its `SharedLlmProvider` handle. Moved here from
//! `openab-agent/src/llm.rs` so the MCP runtime (sampling, meta-tool) can
//! live in this crate; concrete providers (Anthropic/OpenAI/xAI/…) remain
//! in `openab-agent`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

/// A content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Tool definition sent to the LLM.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Events streamed back from the LLM.
#[derive(Debug, Clone)]
pub enum LlmEvent {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Stop,
    #[allow(dead_code)]
    Error(String),
}

/// Trait for LLM providers.
pub trait LlmProvider: Send + Sync {
    fn chat<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>>;

    /// Identifier of the model this provider talks to. Surfaced as
    /// `CreateMessageResult.model` when serving MCP sampling so the requesting
    /// server learns which model produced the response.
    fn model(&self) -> &str;

    /// True if this provider authenticates via OAuth rather than an API key.
    /// Lets a session rebuild (model switch) preserve its auth mode instead of
    /// silently falling back to `ANTHROPIC_API_KEY`.
    fn is_oauth(&self) -> bool {
        false
    }

    /// Canonical provider family name (`anthropic` / `openai` / `xai`).
    /// Combined with [`is_oauth`](Self::is_oauth) so a model switch preserves
    /// auth mode *per provider*: an xAI OAuth session must not make a switch
    /// to Anthropic bypass a configured `ANTHROPIC_API_KEY` (review F2).
    fn provider_name(&self) -> &str {
        ""
    }
}

/// Shared, cloneable handle to an `LlmProvider`. A newtype over
/// `Arc<dyn LlmProvider>` purely so structs that hold one (the MCP runtime
/// manager + per-connection client handler) can keep deriving `Debug` —
/// `dyn LlmProvider` is not `Debug`, so the derive would otherwise fail.
#[derive(Clone)]
pub struct SharedLlmProvider(pub Arc<dyn LlmProvider>);

impl std::fmt::Debug for SharedLlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedLlmProvider(..)")
    }
}

impl std::ops::Deref for SharedLlmProvider {
    type Target = dyn LlmProvider;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

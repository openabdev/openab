use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

/// Provider prefixes [`ModelRef::parse`] recognizes. A `prefix/rest` model
/// splits into `(provider, model)` ONLY when `prefix` is one of these. Otherwise
/// the whole string is the model id — so a HuggingFace-style `org/model` id
/// (e.g. `meta-llama/Llama-3-8B`) for a custom/OpenAI-compatible endpoint stays
/// intact instead of mis-parsing `org` as a provider. Extend as vendors land.
const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "anthropic-oauth",
    "claude",
    "openai",
    "codex",
    "xai",
    "grok",
];

/// A model reference, optionally provider-qualified. Accepts the canonical
/// `provider/model_id` form (e.g. `anthropic/claude-sonnet-4-6`) as well as a
/// bare `model_id` (provider then inferred from credentials). Only a *known*
/// provider prefix is split off (see [`KNOWN_PROVIDERS`]), so model ids that
/// themselves contain `/` (HuggingFace `org/model`) are preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: Option<String>,
    pub model: String,
}

impl ModelRef {
    pub fn parse(input: &str) -> Self {
        match input.split_once('/') {
            Some((p, m)) if KNOWN_PROVIDERS.contains(&p) && !m.is_empty() => ModelRef {
                provider: Some(p.to_string()),
                model: m.to_string(),
            },
            _ => ModelRef {
                provider: None,
                model: input.to_string(),
            },
        }
    }
}

/// The provider the user asked for. Precedence: explicit `OPENAB_AGENT_PROVIDER`
/// → `provider/` prefix of `OPENAB_AGENT_MODEL` (e.g. `openai/gpt-5.4` selects
/// OpenAI even when an Anthropic key is also present) → `provider/` prefix of
/// `config.json`'s `model` → empty (auto-detect). Env-over-config (ADR §5.5).
pub fn resolve_provider_choice() -> String {
    if let Ok(p) = std::env::var("OPENAB_AGENT_PROVIDER") {
        if !p.is_empty() {
            return p;
        }
    }
    if let Some(p) = std::env::var("OPENAB_AGENT_MODEL")
        .ok()
        .and_then(|m| ModelRef::parse(&m).provider)
    {
        return p;
    }
    crate::config::AgentConfig::load_or_default()
        .model
        .and_then(|m| ModelRef::parse(&m).provider)
        .unwrap_or_default()
}

/// Select an `LlmProvider` from an explicit `choice` (`anthropic` /
/// `anthropic-oauth` / `openai` / `codex`) or, for any other value, auto-detect
/// (Anthropic API key, then Claude subscription OAuth, then codex OAuth). The
/// `anthropic` choice itself auto-falls-back from API key to OAuth. Shared by
/// the ACP session path and MCP sampling so both honor the same
/// `OPENAB_AGENT_PROVIDER` selection and credential fallback.
pub fn select_provider(choice: &str) -> Result<Box<dyn LlmProvider>, String> {
    match choice {
        "anthropic" => Ok(Box::new(AnthropicProvider::auto()?)),
        "anthropic-oauth" | "claude" => Ok(Box::new(AnthropicProvider::from_oauth_auto()?)),
        "openai" | "codex" => Ok(Box::new(OpenAiProvider::from_auth_store()?)),
        "xai" | "grok" => Ok(Box::new(XaiProvider::from_auth_store()?)),
        _ => match AnthropicProvider::auto() {
            Ok(p) => Ok(Box::new(p)),
            // F3 — don't let a *present-but-misconfigured* Anthropic credential
            // silently fall through to Codex. If a credential exists, the failure
            // is a real config error (e.g. a credential set but no model): fail
            // loud with it. Only fall through to Codex when no Anthropic
            // credential exists at all.
            Err(anthropic_err) => {
                if AnthropicProvider::credential_present() {
                    Err(format!(
                        "Anthropic credential present but unusable: {anthropic_err}"
                    ))
                } else {
                    OpenAiProvider::from_auth_store()
                        .map(|p| Box::new(p) as Box<dyn LlmProvider>)
                        .map_err(|codex_err| format!(
                            "No credentials: set ANTHROPIC_API_KEY / CLAUDE_CODE_OAUTH_TOKEN, or run `openab-agent auth anthropic-oauth` / `openab-agent auth codex-oauth` / `openab-agent auth xai-device`. ({codex_err})"
                        ))
                }
            }
        },
    }
}

/// Build the default shared provider for non-session background use (MCP
/// sampling). Honors `OPENAB_AGENT_PROVIDER`; returns `None` when no
/// credentials are available so the caller can simply decline to advertise
/// the `sampling` capability rather than fail.
pub fn default_provider() -> Option<SharedLlmProvider> {
    let choice = resolve_provider_choice();
    select_provider(&choice)
        .ok()
        .map(|b| SharedLlmProvider(Arc::from(b)))
}

/// How an `AnthropicProvider` authenticates to the Messages API
/// (credential-source precedence per ADR §5.3).
enum AnthropicAuth {
    /// `ANTHROPIC_API_KEY` → `x-api-key`, plain system prompt.
    ApiKey(String),
    /// Claude Pro/Max subscription OAuth → `Bearer` + Claude Code identity
    /// headers/system block. The live token is fetched (and refreshed) per call
    /// from the `anthropic-oauth` tenant in auth.json.
    OAuth,
    /// Pre-provisioned long-lived subscription OAuth token via
    /// `CLAUDE_CODE_OAUTH_TOKEN` (ADR §5.3 fleet route). Same `Bearer` + Claude
    /// Code identity path as `OAuth`, but the token comes from the env, never
    /// touches `auth.json`, and is never refreshed (ops re-mints it).
    OAuthEnv(String),
}

/// Anthropic Claude provider.
pub struct AnthropicProvider {
    auth: AnthropicAuth,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

/// Resolve the Anthropic model. Precedence (ADR §5.3/§5.5): `OPENAB_AGENT_MODEL`
/// env → `model` in `config.json` → error. No hardcoded default: dateless 4.6+
/// IDs are fixed canonical IDs (not evergreen pointers), so a pinned default is a
/// per-generation 404 timebomb. Require an explicit choice and fail loud instead.
fn anthropic_model() -> Result<String, String> {
    if let Ok(m) = std::env::var("OPENAB_AGENT_MODEL") {
        if !m.is_empty() {
            return Ok(m);
        }
    }
    if let Some(m) = crate::config::AgentConfig::load_or_default().model {
        if !m.is_empty() {
            return Ok(m);
        }
    }
    Err("no model configured; set OPENAB_AGENT_MODEL, add `model` to config.json, or select a model".to_string())
}

/// Max output tokens: `OPENAB_AGENT_MAX_TOKENS` env → `max_tokens` in
/// `config.json` → built-in 8192 (env-over-config, ADR §5.5).
fn anthropic_max_tokens() -> u32 {
    if let Some(v) = std::env::var("OPENAB_AGENT_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return v;
    }
    if let Some(v) = crate::config::AgentConfig::load_or_default().max_tokens {
        return v;
    }
    8192
}

/// openab-agent's built-in tools mapped to Claude Code's canonical casing. The
/// `claude-code-20250219` beta (sent with OAuth tokens) expects these names, so
/// they're rewritten on the way out and restored on the way back. Unknown names
/// (e.g. MCP tools) pass through unchanged, matching Pi's behaviour.
const CC_TOOL_NAMES: &[(&str, &str)] = &[
    ("read", "Read"),
    ("write", "Write"),
    ("edit", "Edit"),
    ("bash", "Bash"),
];

fn to_claude_code_name(name: &str) -> String {
    CC_TOOL_NAMES
        .iter()
        .find(|(lc, _)| *lc == name)
        .map(|(_, cc)| (*cc).to_string())
        .unwrap_or_else(|| name.to_string())
}

fn from_claude_code_name(name: &str) -> String {
    CC_TOOL_NAMES
        .iter()
        .find(|(_, cc)| *cc == name)
        .map(|(lc, _)| (*lc).to_string())
        .unwrap_or_else(|| name.to_string())
}

impl AnthropicProvider {
    fn build(auth: AnthropicAuth, model: String) -> Self {
        Self {
            auth,
            // Accept a provider-qualified ref (`anthropic/claude-…`); the API
            // wants the bare model id.
            model: ModelRef::parse(&model).model,
            max_tokens: anthropic_max_tokens(),
            client: reqwest::Client::new(),
        }
    }

    fn api_key_from_env() -> Result<String, String> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| "ANTHROPIC_API_KEY not set".to_string())?;
        if api_key.is_empty() {
            return Err("ANTHROPIC_API_KEY is empty".to_string());
        }
        Ok(api_key)
    }

    /// Verify the `anthropic-oauth` tenant has a stored token; the live token is
    /// fetched (and refreshed) at call time.
    fn ensure_oauth_token() -> Result<(), String> {
        crate::auth::load_tokens_for(crate::auth::ANTHROPIC_NAMESPACE)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Claude Pro/Max OAuth.
    pub fn from_oauth_store() -> Result<Self, String> {
        Self::ensure_oauth_token()?;
        Ok(Self::build(AnthropicAuth::OAuth, anthropic_model()?))
    }

    /// Pre-provisioned long-lived subscription OAuth token from
    /// `CLAUDE_CODE_OAUTH_TOKEN` (ADR §5.3). No `auth.json`, no refresh.
    fn oauth_env_token() -> Option<String> {
        std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
    }

    /// Build from the `CLAUDE_CODE_OAUTH_TOKEN` env route.
    pub fn from_oauth_env() -> Result<Self, String> {
        let token =
            Self::oauth_env_token().ok_or_else(|| "CLAUDE_CODE_OAUTH_TOKEN not set".to_string())?;
        Ok(Self::build(
            AnthropicAuth::OAuthEnv(token),
            anthropic_model()?,
        ))
    }

    fn from_oauth_env_with_model(model: &str) -> Result<Self, String> {
        let token =
            Self::oauth_env_token().ok_or_else(|| "CLAUDE_CODE_OAUTH_TOKEN not set".to_string())?;
        Ok(Self::build(
            AnthropicAuth::OAuthEnv(token),
            model.to_string(),
        ))
    }

    /// True when *some* Anthropic credential source exists (API key, env OAuth
    /// token, or stored tenant). Lets `select_provider` tell a real config error
    /// ("credential present but `auto()` failed" → fail loud) from "no Anthropic
    /// credentials" (legitimately fall through to Codex) — review F3.
    pub fn credential_present() -> bool {
        Self::api_key_from_env().is_ok()
            || Self::oauth_env_token().is_some()
            || crate::auth::load_tokens_for(crate::auth::ANTHROPIC_NAMESPACE).is_ok()
    }

    /// Apply the Claude Pro/Max OAuth `Bearer` + Claude Code identity headers.
    /// Shared by the stored-tenant (`OAuth`) and env-token (`OAuthEnv`) paths.
    fn oauth_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        req.header("authorization", format!("Bearer {token}"))
            .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
            .header("user-agent", "claude-cli/1.0.0")
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true")
    }

    /// Credential-source precedence (ADR §5.3): explicit `ANTHROPIC_API_KEY` →
    /// pre-provisioned `CLAUDE_CODE_OAUTH_TOKEN` env route → stored interactive
    /// `anthropic-oauth` tenant. When a source is present its own errors (e.g. a
    /// missing model) surface rather than falling through to an unrelated
    /// lower-precedence credential error.
    pub fn auto() -> Result<Self, String> {
        if let Ok(key) = Self::api_key_from_env() {
            return Ok(Self::build(AnthropicAuth::ApiKey(key), anthropic_model()?));
        }
        if Self::oauth_env_token().is_some() {
            return Self::from_oauth_env();
        }
        Self::from_oauth_store()
    }

    /// `auto()` with an explicit model override. The override replaces
    /// `OPENAB_AGENT_MODEL`, so it does not require that env var to be set.
    pub fn auto_with_model(model: &str) -> Result<Self, String> {
        if let Ok(key) = Self::api_key_from_env() {
            return Ok(Self::build(AnthropicAuth::ApiKey(key), model.to_string()));
        }
        if Self::oauth_env_token().is_some() {
            return Self::from_oauth_env_with_model(model);
        }
        Self::from_oauth_store_with_model(model)
    }

    /// `from_oauth_store()` with an explicit model override.
    pub fn from_oauth_store_with_model(model: &str) -> Result<Self, String> {
        Self::ensure_oauth_token()?;
        Ok(Self::build(AnthropicAuth::OAuth, model.to_string()))
    }

    /// OAuth with env-over-store precedence: `CLAUDE_CODE_OAUTH_TOKEN` → stored
    /// `anthropic-oauth` tenant. Lets fleet pods that only set the env token work
    /// without an `auth.json`.
    pub fn from_oauth_auto() -> Result<Self, String> {
        if Self::oauth_env_token().is_some() {
            return Self::from_oauth_env();
        }
        Self::from_oauth_store()
    }

    /// `from_oauth_auto()` with an explicit model override.
    pub fn from_oauth_auto_with_model(model: &str) -> Result<Self, String> {
        if Self::oauth_env_token().is_some() {
            return Self::from_oauth_env_with_model(model);
        }
        Self::from_oauth_store_with_model(model)
    }

    fn build_request_body(&self, system: &str, messages: &[Message], tools: &[ToolDef]) -> Value {
        let oauth = self.is_oauth();
        let msgs: Vec<Value> =
            messages
                .iter()
                .map(|m| {
                    let content: Vec<Value> = m
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
                        ContentBlock::ToolUse { id, name, input } => {
                            let name = if oauth { to_claude_code_name(name) } else { name.clone() };
                            json!({ "type": "tool_use", "id": id, "name": name, "input": input })
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let mut v = json!({
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content
                            });
                            if let Some(true) = is_error {
                                v["is_error"] = json!(true);
                            }
                            v
                        }
                    })
                    .collect();
                    json!({ "role": &m.role, "content": content })
                })
                .collect();

        let mut body = json!({
            "model": &self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
        });

        // OAuth tokens MUST carry the Claude Code identity as the first system
        // block, with the real prompt appended. API-key callers send a plain
        // string (unchanged behaviour).
        if oauth {
            body["system"] = json!([
                { "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." },
                { "type": "text", "text": system },
            ]);
        } else {
            body["system"] = json!(system);
        }

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let name = if oauth {
                        to_claude_code_name(&t.name)
                    } else {
                        t.name.clone()
                    };
                    json!({
                        "name": name,
                        "description": &t.description,
                        "input_schema": &t.input_schema
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        body
    }
}

impl LlmProvider for AnthropicProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn is_oauth(&self) -> bool {
        matches!(self.auth, AnthropicAuth::OAuth | AnthropicAuth::OAuthEnv(_))
    }

    fn chat<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>> {
        Box::pin(async move {
            let body = self.build_request_body(system, messages, tools);
            let oauth = self.is_oauth();
            // Only the stored `anthropic-oauth` tenant can be refreshed on a 401;
            // the `CLAUDE_CODE_OAUTH_TOKEN` env route has no tenant to refresh
            // (a 401 there means the pre-provisioned token is bad → surface it).
            let refreshable = matches!(self.auth, AnthropicAuth::OAuth);
            let max_retries = 3u32;
            let mut oauth_refreshed = false;

            for attempt in 0..=max_retries {
                let mut req = self
                    .client
                    .post("https://api.anthropic.com/v1/messages")
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json");
                req = match &self.auth {
                    AnthropicAuth::ApiKey(key) => req.header("x-api-key", key),
                    AnthropicAuth::OAuth => {
                        // Claude Pro/Max: live token from the stored tenant.
                        let token =
                            crate::auth::get_valid_token_for(crate::auth::ANTHROPIC_NAMESPACE)
                                .await?;
                        Self::oauth_headers(req, &token)
                    }
                    AnthropicAuth::OAuthEnv(token) => Self::oauth_headers(req, token),
                };

                let resp = req
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow!("HTTP request failed: {e}"))?;

                let status = resp.status();

                // Retry on 429 (rate limit) or 529 (overloaded)
                if (status.as_u16() == 429 || status.as_u16() == 529) && attempt < max_retries {
                    let delay = std::time::Duration::from_millis(1000 * 2u64.pow(attempt));
                    tokio::time::sleep(delay).await;
                    continue;
                }

                // 401 on OAuth: token may have expired mid-request; force a
                // refresh and retry once. Surface a failed refresh instead of
                // retrying with the stale token.
                if refreshable && status.as_u16() == 401 && !oauth_refreshed {
                    oauth_refreshed = true;
                    crate::auth::force_refresh_for(crate::auth::ANTHROPIC_NAMESPACE).await?;
                    continue;
                }

                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(anyhow!("Anthropic API error {status}: {text}"));
                }

                let response: Value = resp
                    .json()
                    .await
                    .map_err(|e| anyhow!("Failed to parse response: {e}"))?;

                let mut events = parse_anthropic_response(&response)?;
                // Restore openab-agent's lowercase tool names from the Claude
                // Code canonical casing the model echoes back under OAuth.
                if oauth {
                    for ev in &mut events {
                        if let LlmEvent::ToolUse { name, .. } = ev {
                            *name = from_claude_code_name(name);
                        }
                    }
                }
                return Ok(events);
            }

            Err(anyhow!("Anthropic API: max retries exceeded"))
        })
    }
}

fn parse_anthropic_response(response: &Value) -> Result<Vec<LlmEvent>> {
    let mut events = Vec::new();

    let content = response
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing content in response"))?;

    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    events.push(LlmEvent::Text(text.to_string()));
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                events.push(LlmEvent::ToolUse { id, name, input });
            }
            _ => {}
        }
    }

    let stop_reason = response
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .unwrap_or("end_turn");

    if stop_reason != "tool_use" {
        events.push(LlmEvent::Stop);
    }

    Ok(events)
}

// === OpenAI-compatible Provider (for Codex subscription via OAuth) ===

pub struct OpenAiProvider {
    base_url: String,
    model: String,
    #[allow(dead_code)]
    max_tokens: u32,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create provider using stored OAuth token from ~/.openab/agent/auth.json
    pub fn from_auth_store() -> Result<Self, String> {
        // Just verify tokens exist; actual token is fetched at call time
        crate::auth::load_tokens().map_err(|e| e.to_string())?;
        Ok(Self {
            base_url: std::env::var("OPENAB_AGENT_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api".to_string()),
            model: ModelRef::parse(
                &std::env::var("OPENAB_AGENT_OPENAI_MODEL")
                    .or_else(|_| std::env::var("OPENAB_AGENT_MODEL"))
                    .unwrap_or_else(|_| "gpt-5.4-mini".to_string()),
            )
            .model,
            max_tokens: std::env::var("OPENAB_AGENT_MAX_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8192),
            client: reqwest::Client::new(),
        })
    }

    /// Create provider with a specific model override.
    pub fn from_auth_store_with_model(model: &str) -> Result<Self, String> {
        let mut p = Self::from_auth_store()?;
        p.model = ModelRef::parse(model).model;
        Ok(p)
    }
}

impl LlmProvider for OpenAiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn chat<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>> {
        Box::pin(async move {
            // Build Responses API input format
            let mut oai_messages: Vec<Value> = vec![];
            for m in messages {
                if m.role == "user" {
                    // User text messages
                    let texts: Vec<&str> = m
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !texts.is_empty() {
                        oai_messages.push(json!({"role": "user", "content": [{"type": "input_text", "text": texts.join("")}]}));
                    }
                    // Tool results as function_call_output
                    for b in &m.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = b
                        {
                            oai_messages.push(json!({"type": "function_call_output", "call_id": tool_use_id, "output": content}));
                        }
                    }
                } else if m.role == "assistant" {
                    for b in &m.content {
                        match b {
                            ContentBlock::Text { text } => {
                                oai_messages.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text, "annotations": []}]}));
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                oai_messages.push(json!({"type": "function_call", "call_id": id, "name": name, "arguments": input.to_string()}));
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Build Responses API body
            let mut body = json!({
                "model": &self.model,
                "store": false,
                "stream": true,
                "instructions": system,
                "input": oai_messages,
                "tool_choice": "auto",
                "parallel_tool_calls": true,
            });

            if !tools.is_empty() {
                let resp_tools: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": &t.name,
                            "description": &t.description,
                            "parameters": &t.input_schema
                        })
                    })
                    .collect();
                body["tools"] = json!(resp_tools);
            }

            let max_retries = 3u32;
            for attempt in 0..=max_retries {
                let token = crate::auth::get_valid_token().await?;
                // Extract account ID from JWT for chatgpt backend API
                let account_id = extract_account_id_from_jwt(&token);
                let mut req = self
                    .client
                    .post(format!("{}/codex/responses", self.base_url))
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .header("originator", "openab-agent");
                if let Some(ref aid) = account_id {
                    req = req.header("chatgpt-account-id", aid);
                }
                let resp = req
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow!("HTTP request failed: {e}"))?;

                let status = resp.status();
                if (status.as_u16() == 429 || status.as_u16() == 529) && attempt < max_retries {
                    let delay = std::time::Duration::from_millis(1000 * 2u64.pow(attempt));
                    tokio::time::sleep(delay).await;
                    continue;
                }

                // 401: token may have expired mid-request, force refresh and retry
                if status.as_u16() == 401 && attempt < max_retries {
                    let _ = crate::auth::force_refresh().await;
                    continue;
                }

                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(anyhow!("OpenAI API error {status}: {text}"));
                }

                // Parse SSE stream - collect output items from response.output_item.done events
                let text = resp
                    .text()
                    .await
                    .map_err(|e| anyhow!("Failed to read response: {e}"))?;
                let mut output_items: Vec<Value> = Vec::new();
                for line in text.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            break;
                        }
                        if let Ok(event) = serde_json::from_str::<Value>(data) {
                            let event_type =
                                event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if event_type == "response.output_item.done" {
                                if let Some(item) = event.get("item") {
                                    output_items.push(item.clone());
                                }
                            }
                        }
                    }
                }
                if output_items.is_empty() {
                    return Err(anyhow!(
                        "No output items in SSE stream. Raw: {}",
                        &text[..text.len().min(500)]
                    ));
                }
                let response = json!({"output": output_items});
                return parse_openai_response(&response);
            }
            Err(anyhow!("OpenAI API: max retries exceeded"))
        })
    }
}

// === xAI Provider (SuperGrok / X Premium subscription via device OAuth) ===
//
// xAI's API is OpenAI Chat Completions-compatible at `api.x.ai/v1`; the OAuth
// access token from the `xai-oauth` tenant acts as a plain Bearer API key
// (scope `api:access`), so no bespoke wire format is needed — requests go to
// `/chat/completions` and responses reuse `parse_openai_response`'s
// Chat Completions path.

pub struct XaiProvider {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl XaiProvider {
    /// Create provider using the stored xAI OAuth token from
    /// `~/.openab/agent/auth.json` (run `openab-agent auth xai-device` first).
    pub fn from_auth_store() -> Result<Self, String> {
        // Just verify tokens exist; the live token is fetched (and refreshed)
        // per call, mirroring `OpenAiProvider`.
        crate::auth::load_tokens_for(crate::auth::XAI_NAMESPACE).map_err(|e| e.to_string())?;
        Ok(Self {
            base_url: std::env::var("OPENAB_AGENT_XAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.x.ai/v1".to_string()),
            model: ModelRef::parse(
                &std::env::var("OPENAB_AGENT_XAI_MODEL")
                    .or_else(|_| std::env::var("OPENAB_AGENT_MODEL"))
                    .unwrap_or_else(|_| "grok-4.5".to_string()),
            )
            .model,
            client: reqwest::Client::new(),
        })
    }

    /// Create provider with a specific model override.
    pub fn from_auth_store_with_model(model: &str) -> Result<Self, String> {
        let mut p = Self::from_auth_store()?;
        p.model = ModelRef::parse(model).model;
        Ok(p)
    }
}

/// Convert the internal transcript to Chat Completions `messages`. Pure so the
/// mapping (tool_use → assistant `tool_calls`, tool_result → `role: tool`) is
/// unit-testable. Tool results are emitted *before* any user text from the same
/// message: Chat Completions requires `tool` messages to directly follow the
/// assistant message carrying the corresponding `tool_calls`.
fn xai_chat_messages(system: &str, messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![json!({"role": "system", "content": system})];
    for m in messages {
        if m.role == "user" {
            for b in &m.content {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } = b
                {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content,
                    }));
                }
            }
            let texts: Vec<&str> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !texts.is_empty() {
                out.push(json!({"role": "user", "content": texts.join("")}));
            }
        } else if m.role == "assistant" {
            let mut text_parts: Vec<&str> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text } => text_parts.push(text.as_str()),
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": input.to_string()},
                        }));
                    }
                    _ => {}
                }
            }
            if text_parts.is_empty() && tool_calls.is_empty() {
                continue;
            }
            let mut msg = json!({"role": "assistant"});
            msg["content"] = if text_parts.is_empty() {
                Value::Null
            } else {
                Value::String(text_parts.join(""))
            };
            if !tool_calls.is_empty() {
                msg["tool_calls"] = json!(tool_calls);
            }
            out.push(msg);
        }
    }
    out
}

impl LlmProvider for XaiProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn is_oauth(&self) -> bool {
        true
    }

    fn chat<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDef],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>> {
        Box::pin(async move {
            let mut body = json!({
                "model": &self.model,
                "messages": xai_chat_messages(system, messages),
                "stream": false,
            });
            if !tools.is_empty() {
                let cc_tools: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": &t.name,
                                "description": &t.description,
                                "parameters": &t.input_schema,
                            }
                        })
                    })
                    .collect();
                body["tools"] = json!(cc_tools);
                body["tool_choice"] = json!("auto");
            }

            let max_retries = 3u32;
            for attempt in 0..=max_retries {
                let token = crate::auth::get_valid_token_for(crate::auth::XAI_NAMESPACE).await?;
                let resp = self
                    .client
                    .post(format!("{}/chat/completions", self.base_url))
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow!("HTTP request failed: {e}"))?;

                let status = resp.status();
                if (status.as_u16() == 429 || status.as_u16() == 529) && attempt < max_retries {
                    let delay = std::time::Duration::from_millis(1000 * 2u64.pow(attempt));
                    tokio::time::sleep(delay).await;
                    continue;
                }

                // 401: token may have expired mid-request, force refresh and retry
                if status.as_u16() == 401 && attempt < max_retries {
                    let _ = crate::auth::force_refresh_for(crate::auth::XAI_NAMESPACE).await;
                    continue;
                }

                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(anyhow!("xAI API error {status}: {text}"));
                }

                let payload: Value = resp
                    .json()
                    .await
                    .map_err(|e| anyhow!("Failed to parse xAI response: {e}"))?;
                // Chat Completions shape → parse_openai_response's fallback path.
                return parse_openai_response(&payload);
            }
            Err(anyhow!("xAI API: max retries exceeded"))
        })
    }
}

fn extract_account_id_from_jwt(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut payload = parts[1].to_string();
    while !payload.len().is_multiple_of(4) {
        payload.push('=');
    }
    let decoded = base64::engine::general_purpose::URL_SAFE
        .decode(&payload)
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD
                .decode(&payload)
                .ok()
        })?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .map(|s| s.to_string())
}

fn parse_openai_response(response: &Value) -> Result<Vec<LlmEvent>> {
    let mut events = Vec::new();

    // Handle Responses API format (output array)
    if let Some(output) = response.get("output").and_then(|o| o.as_array()) {
        for item in output {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            if block.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    events.push(LlmEvent::Text(text.to_string()));
                                }
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args_str = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                    events.push(LlmEvent::ToolUse { id, name, input });
                }
                _ => {}
            }
        }
        events.push(LlmEvent::Stop);
        return Ok(events);
    }

    // Fallback: Chat Completions format
    let choice = response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow!("No choices in response"))?;

    let message = choice.get("message").ok_or_else(|| anyhow!("No message"))?;

    // Text content
    if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
        if !content.is_empty() {
            events.push(LlmEvent::Text(content.to_string()));
        }
    }

    // Tool calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
            events.push(LlmEvent::ToolUse { id, name, input });
        }
    }

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop");
    if finish_reason != "tool_calls" {
        events.push(LlmEvent::Stop);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_ref_parse() {
        // Provider-qualified form splits on the first slash.
        let r = ModelRef::parse("anthropic/claude-sonnet-4-6");
        assert_eq!(r.provider.as_deref(), Some("anthropic"));
        assert_eq!(r.model, "claude-sonnet-4-6");

        // Bare model id → no provider, model unchanged.
        let r = ModelRef::parse("claude-sonnet-4-6");
        assert_eq!(r.provider, None);
        assert_eq!(r.model, "claude-sonnet-4-6");

        // Degenerate slashes fall back to bare (no empty provider/model).
        assert_eq!(ModelRef::parse("/gpt-5.4").provider, None);
        assert_eq!(ModelRef::parse("openai/").model, "openai/");

        // F4: a HuggingFace-style `org/model` id is NOT a known provider, so the
        // whole string stays the model id (the `/` is part of the id).
        let r = ModelRef::parse("meta-llama/Llama-3-8B-Instruct");
        assert_eq!(r.provider, None);
        assert_eq!(r.model, "meta-llama/Llama-3-8B-Instruct");

        // Every known provider prefix still splits.
        for prov in KNOWN_PROVIDERS {
            let r = ModelRef::parse(&format!("{prov}/some-model"));
            assert_eq!(r.provider.as_deref(), Some(*prov));
            assert_eq!(r.model, "some-model");
        }
    }

    #[test]
    fn test_provider_build_strips_prefix() {
        // A qualified ref reaches the API as the bare model id.
        let p = AnthropicProvider::build(
            AnthropicAuth::ApiKey("k".to_string()),
            "anthropic/claude-opus-4-8".to_string(),
        );
        assert_eq!(p.model(), "claude-opus-4-8");
    }

    #[test]
    fn test_parse_text_response() {
        let resp = json!({
            "content": [{"type": "text", "text": "Hello world"}],
            "stop_reason": "end_turn"
        });
        let events = parse_anthropic_response(&resp).unwrap();
        assert_eq!(events.len(), 2);
        match &events[0] {
            LlmEvent::Text(t) => assert_eq!(t, "Hello world"),
            _ => panic!("expected Text event"),
        }
        assert!(matches!(events[1], LlmEvent::Stop));
    }

    #[test]
    fn test_parse_tool_use_response() {
        let resp = json!({
            "content": [
                {"type": "tool_use", "id": "tu_1", "name": "read", "input": {"path": "/tmp/x"}}
            ],
            "stop_reason": "tool_use"
        });
        let events = parse_anthropic_response(&resp).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "/tmp/x");
            }
            _ => panic!("expected ToolUse event"),
        }
    }

    fn test_provider(auth: AnthropicAuth) -> AnthropicProvider {
        AnthropicProvider {
            auth,
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            client: reqwest::Client::new(),
        }
    }

    #[test]
    fn test_is_oauth_reflects_auth_mode() {
        // Guards the ACP model-switch rebuild: an OAuth session must report
        // OAuth so it isn't silently rebuilt against ANTHROPIC_API_KEY. The env
        // route is OAuth too — it uses the same Claude Code identity path.
        assert!(test_provider(AnthropicAuth::OAuth).is_oauth());
        assert!(test_provider(AnthropicAuth::OAuthEnv("oat".to_string())).is_oauth());
        assert!(!test_provider(AnthropicAuth::ApiKey("k".to_string())).is_oauth());
    }

    #[test]
    fn auto_prefers_api_key_over_env_token() {
        // ADR §5.3 precedence: ANTHROPIC_API_KEY wins over CLAUDE_CODE_OAUTH_TOKEN.
        temp_env::with_vars(
            [
                ("ANTHROPIC_API_KEY", Some("sk-ant-test")),
                ("CLAUDE_CODE_OAUTH_TOKEN", Some("oat-test")),
                ("OPENAB_AGENT_MODEL", Some("anthropic/claude-sonnet-4-6")),
            ],
            || {
                let p = AnthropicProvider::auto().unwrap();
                assert!(matches!(p.auth, AnthropicAuth::ApiKey(_)));
            },
        );
    }

    #[test]
    fn auto_uses_env_token_when_no_api_key() {
        // No API key → the CLAUDE_CODE_OAUTH_TOKEN env route, not the stored tenant
        // (this builds without reading auth.json).
        temp_env::with_vars(
            [
                ("ANTHROPIC_API_KEY", None),
                ("CLAUDE_CODE_OAUTH_TOKEN", Some("oat-test")),
                ("OPENAB_AGENT_MODEL", Some("anthropic/claude-sonnet-4-6")),
            ],
            || {
                let p = AnthropicProvider::auto().unwrap();
                assert!(matches!(p.auth, AnthropicAuth::OAuthEnv(_)));
                assert!(p.is_oauth());
            },
        );
    }

    #[test]
    fn model_resolves_env_over_config_over_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, r#"{"model":"anthropic/from-config"}"#).unwrap();
        let cfg_path = cfg.to_str().unwrap();

        // env wins over config.json
        temp_env::with_vars(
            [
                ("OPENAB_CONFIG_PATH", Some(cfg_path)),
                ("OPENAB_AGENT_MODEL", Some("anthropic/from-env")),
            ],
            || {
                assert_eq!(anthropic_model().unwrap(), "anthropic/from-env");
                assert_eq!(resolve_provider_choice(), "anthropic");
            },
        );

        // no env → config.json supplies the model (and its provider prefix)
        temp_env::with_vars(
            [
                ("OPENAB_CONFIG_PATH", Some(cfg_path)),
                ("OPENAB_AGENT_MODEL", None),
                ("OPENAB_AGENT_PROVIDER", None),
            ],
            || {
                assert_eq!(anthropic_model().unwrap(), "anthropic/from-config");
                assert_eq!(resolve_provider_choice(), "anthropic");
            },
        );

        // neither env nor config → fail loud
        temp_env::with_vars(
            [
                (
                    "OPENAB_CONFIG_PATH",
                    Some(dir.path().join("missing.json").to_str().unwrap()),
                ),
                ("OPENAB_AGENT_MODEL", None),
            ],
            || assert!(anthropic_model().is_err()),
        );
    }

    #[test]
    fn select_provider_fails_loud_on_misconfigured_anthropic() {
        // F3: an Anthropic credential is present (API key) but no model is set, so
        // auto() fails for a config reason. select_provider must surface that
        // error, not silently fall through to Codex.
        temp_env::with_vars(
            [
                ("ANTHROPIC_API_KEY", Some("sk-ant-test")),
                ("CLAUDE_CODE_OAUTH_TOKEN", None),
                ("OPENAB_AGENT_MODEL", None),
                ("OPENAB_AGENT_PROVIDER", None),
            ],
            || {
                // `Box<dyn LlmProvider>` isn't Debug, so match rather than unwrap_err.
                let err = match select_provider("") {
                    Ok(_) => panic!("expected a fail-loud error, got a provider"),
                    Err(e) => e,
                };
                assert!(err.contains("present but unusable"), "got: {err}");
            },
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = test_provider(AnthropicAuth::ApiKey("test".to_string()));
        let messages = vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
            }],
        }];
        let body = provider.build_request_body("system prompt", &messages, &[]);
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["max_tokens"], 4096);
        // API-key mode keeps the plain-string system prompt.
        assert_eq!(body["system"], "system prompt");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn test_build_request_body_oauth_injects_claude_code_identity_and_caps_tools() {
        let provider = test_provider(AnthropicAuth::OAuth);
        let messages = vec![Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "tu_1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "/tmp/x"}),
            }],
        }];
        let tools = vec![ToolDef {
            name: "bash".to_string(),
            description: "run".to_string(),
            input_schema: json!({}),
        }];
        let body = provider.build_request_body("real prompt", &messages, &tools);
        // system[0] must be the Claude Code identity, real prompt appended.
        assert_eq!(
            body["system"][0]["text"],
            "You are Claude Code, Anthropic's official CLI for Claude."
        );
        assert_eq!(body["system"][1]["text"], "real prompt");
        // tool def + assistant tool_use names normalised to CC casing.
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(body["messages"][0]["content"][0]["name"], "Read");
    }

    #[test]
    fn test_claude_code_name_round_trip_and_passthrough() {
        assert_eq!(to_claude_code_name("read"), "Read");
        assert_eq!(from_claude_code_name("Read"), "read");
        // unknown (e.g. MCP) names pass through unchanged both ways.
        assert_eq!(to_claude_code_name("linear_search"), "linear_search");
        assert_eq!(from_claude_code_name("linear_search"), "linear_search");
    }

    #[test]
    fn test_parse_openai_text_response() {
        let resp = json!({
            "choices": [{"message": {"content": "Hello"}, "finish_reason": "stop"}]
        });
        let events = parse_openai_response(&resp).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], LlmEvent::Text(t) if t == "Hello"));
        assert!(matches!(events[1], LlmEvent::Stop));
    }

    #[test]
    fn test_parse_openai_tool_call_response() {
        let resp = json!({
            "choices": [{"message": {
                "content": null,
                "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"x.txt\"}"}}]
            }, "finish_reason": "tool_calls"}]
        });
        let events = parse_openai_response(&resp).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "x.txt");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn test_parse_openai_empty_choices() {
        let resp = json!({"choices": []});
        assert!(parse_openai_response(&resp).is_err());
    }

    #[test]
    fn test_model_ref_parses_xai_and_grok_prefixes() {
        let r = ModelRef::parse("xai/grok-4.5");
        assert_eq!(r.provider.as_deref(), Some("xai"));
        assert_eq!(r.model, "grok-4.5");
        let r = ModelRef::parse("grok/grok-4.3");
        assert_eq!(r.provider.as_deref(), Some("grok"));
        assert_eq!(r.model, "grok-4.3");
        // Bare grok model id: no provider split.
        let r = ModelRef::parse("grok-4.5");
        assert_eq!(r.provider, None);
        assert_eq!(r.model, "grok-4.5");
    }

    #[test]
    fn test_xai_chat_messages_maps_transcript_to_chat_completions() {
        let messages = vec![
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "list files".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::Text {
                        text: "Listing.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "bash".to_string(),
                        input: json!({"command": "ls"}),
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![
                    // Text alongside a tool result: the tool message must still
                    // directly follow the assistant tool_calls message.
                    ContentBlock::Text {
                        text: "thanks".to_string(),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: "a.txt".to_string(),
                        is_error: None,
                    },
                ],
            },
        ];
        let out = xai_chat_messages("sys", &messages);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "sys");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"], "list files");
        // Assistant text + tool_calls in one message, arguments stringified.
        assert_eq!(out[2]["role"], "assistant");
        assert_eq!(out[2]["content"], "Listing.");
        assert_eq!(out[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(out[2]["tool_calls"][0]["type"], "function");
        assert_eq!(out[2]["tool_calls"][0]["function"]["name"], "bash");
        assert_eq!(
            out[2]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        // Tool result emitted before the trailing user text (adjacency rule).
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "call_1");
        assert_eq!(out[3]["content"], "a.txt");
        assert_eq!(out[4]["role"], "user");
        assert_eq!(out[4]["content"], "thanks");
    }

    #[test]
    fn test_xai_chat_messages_tool_only_assistant_has_null_content() {
        let messages = vec![Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "call_2".to_string(),
                name: "read".to_string(),
                input: json!({"path": "x"}),
            }],
        }];
        let out = xai_chat_messages("s", &messages);
        assert_eq!(out[1]["role"], "assistant");
        assert!(out[1]["content"].is_null());
        assert_eq!(out[1]["tool_calls"][0]["function"]["name"], "read");
    }
}

use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::llm::{ContentBlock, LlmEvent, LlmProvider, Message, ToolDef};
use crate::mcp::{self, McpRuntimeManager};
use crate::skills;
use crate::tools;

const SYSTEM_PROMPT: &str = r#"You are openab-agent, a coding assistant. You help users by reading, writing, and editing files, and running shell commands.

You have these core tools available (when MCP servers are configured, an `mcp` tool and their server tools are listed below in addition to these):
- read: Read file contents or list a directory
- write: Create or overwrite a file
- edit: Replace a string in a file (first occurrence)
- bash: Execute a shell command

Be direct and concise. Execute tasks immediately rather than explaining what you would do. When you need to understand code, read the relevant files first."#;

// The MCP system-prompt appendix is generated dynamically by
// `mcp::format_system_prompt_appendix(manager)` so the LLM sees both the
// `mcp` tool intro AND a server catalogue (PR #959 F1 discovery slice).
// Previously a static const here, but that hid the configured server names
// from the LLM and produced the "fs is disconnected, I give up" failure
// mode observed in the F1 PoC.

const DEFAULT_MAX_TOOL_LOOPS: usize = 50;

fn max_tool_loops() -> usize {
    let raw = match std::env::var("OPENAB_AGENT_MAX_TOOL_LOOPS") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    "OPENAB_AGENT_MAX_TOOL_LOOPS={val:?} is not valid ({e}), \
                     falling back to {DEFAULT_MAX_TOOL_LOOPS}"
                );
                DEFAULT_MAX_TOOL_LOOPS
            }
        },
        Err(_) => DEFAULT_MAX_TOOL_LOOPS,
    };
    if raw == 0 {
        warn!(
            "OPENAB_AGENT_MAX_TOOL_LOOPS=0 would prevent the agent from running; \
             using minimum value of 1"
        );
        1
    } else {
        raw
    }
}

/// Maximum number of messages to keep in context. When exceeded, oldest
/// messages (excluding the first user message) are dropped.
const MAX_CONTEXT_MESSAGES: usize = 100;

pub struct Agent {
    provider: Box<dyn LlmProvider>,
    messages: Vec<Message>,
    working_dir: PathBuf,
    system_prompt: String,
    tools: Vec<ToolDef>,
    mcp_manager: Option<McpRuntimeManager>,
    /// Sticky per-session auth policy for Anthropic (review round-3 F2): set
    /// whenever an Anthropic provider is active, and *retained* while other
    /// providers (xAI, Codex) are active, so an Anthropic-OAuth → xAI →
    /// Anthropic round trip switches back to OAuth instead of silently
    /// preferring `ANTHROPIC_API_KEY` (a different account/billing context).
    anthropic_oauth_preferred: bool,
}

/// The sticky-preference update shared by construction and provider swap:
/// only an *active Anthropic* provider rewrites the remembered policy.
fn anthropic_oauth_preference(provider: &dyn LlmProvider, previous: bool) -> bool {
    if provider.provider_name() == "anthropic" {
        provider.is_oauth()
    } else {
        previous
    }
}

impl Agent {
    #[cfg(test)]
    pub fn new(provider: impl LlmProvider + 'static, working_dir: String) -> Self {
        let system_prompt = Self::build_system_prompt(&working_dir, None);
        let anthropic_oauth_preferred = anthropic_oauth_preference(&provider, false);
        Self {
            provider: Box::new(provider),
            messages: Vec::new(),
            working_dir: PathBuf::from(working_dir),
            system_prompt,
            tools: tools::tool_definitions(),
            mcp_manager: None,
            anthropic_oauth_preferred,
        }
    }

    pub fn new_boxed(
        provider: Box<dyn LlmProvider>,
        working_dir: String,
        mcp_manager: Option<McpRuntimeManager>,
    ) -> Self {
        let system_prompt = Self::build_system_prompt(&working_dir, mcp_manager.as_ref());
        let tools = {
            let mut t = tools::tool_definitions();
            if mcp_manager.is_some() {
                t.push(mcp::mcp_tool_def());
            }
            t
        };
        let anthropic_oauth_preferred = anthropic_oauth_preference(provider.as_ref(), false);
        Self {
            provider,
            messages: Vec::new(),
            working_dir: PathBuf::from(working_dir),
            system_prompt,
            tools,
            mcp_manager,
            anthropic_oauth_preferred,
        }
    }

    /// Replace the LLM provider while preserving conversation history (and the
    /// sticky Anthropic auth policy — see `anthropic_oauth_preferred`).
    pub fn swap_provider(&mut self, provider: Box<dyn LlmProvider>) {
        self.anthropic_oauth_preferred =
            anthropic_oauth_preference(provider.as_ref(), self.anthropic_oauth_preferred);
        self.provider = provider;
    }

    /// Sticky Anthropic auth policy for this session (review round-3 F2):
    /// true when the session most recently ran Anthropic in OAuth mode, even
    /// if another provider is active right now.
    pub fn prefers_anthropic_oauth(&self) -> bool {
        self.anthropic_oauth_preferred
    }

    /// The model id the current provider will use. Authoritative source for the
    /// session's reported model (avoids a separate hardcoded default).
    pub fn provider_model(&self) -> String {
        self.provider.model().to_string()
    }

    /// Update working directory and rebuild system prompt.
    pub fn set_working_dir(&mut self, cwd: String) {
        self.system_prompt = Self::build_system_prompt(&cwd, self.mcp_manager.as_ref());
        self.working_dir = PathBuf::from(cwd);
    }

    /// Number of messages in the conversation (test helper).
    #[cfg(test)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Push a message into the conversation (test helper).
    #[cfg(test)]
    pub fn push_message(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// Build the system prompt sent on every LLM call. Composition order:
    ///   1. base prompt (`SYSTEM_PROMPT`, optionally prefixed by project-local
    ///      `AGENTS.md`),
    ///   2. MCP appendix — tool intro + server catalogue (PR #959 F1
    ///      discovery slice); only when `mcp_manager` is `Some`,
    ///   3. skills catalogue.
    ///
    /// Built once at `Agent::new*` time and reused on every `call_llm`.
    fn build_system_prompt(working_dir: &str, mcp_manager: Option<&McpRuntimeManager>) -> String {
        let wd = std::path::Path::new(working_dir);
        let agents_md = wd.join("AGENTS.md");
        let custom = std::fs::read_to_string(&agents_md).unwrap_or_default();

        let base = if custom.is_empty() {
            SYSTEM_PROMPT.to_string()
        } else {
            format!("{}\n\n---\n\n{}", custom.trim(), SYSTEM_PROMPT)
        };

        let base = if let Some(mgr) = mcp_manager {
            format!("{base}{}", mcp::format_system_prompt_appendix(mgr))
        } else {
            base
        };

        let discovered = skills::discover_skills(wd);
        if discovered.is_empty() {
            base
        } else {
            info!("loaded {} skill(s)", discovered.len());
            format!("{}{}", base, skills::format_skills_prompt(&discovered))
        }
    }

    pub async fn run(&mut self, prompt: &str) -> Result<String> {
        // Add user message
        self.messages.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
            }],
        });

        let mut final_text = String::new();
        let max_loops = max_tool_loops();
        if max_loops != DEFAULT_MAX_TOOL_LOOPS {
            info!("max_tool_loops={max_loops} (overridden)");
        } else {
            debug!("max_tool_loops={max_loops}");
        }

        for iteration in 0..max_loops {
            debug!("agent loop iteration {iteration}");

            // Truncate context to prevent unbounded growth / token limit
            self.truncate_context();

            let events = self.call_llm().await?;

            let mut tool_calls = Vec::new();
            let mut text_parts = Vec::new();

            for event in &events {
                match event {
                    LlmEvent::Text(t) => text_parts.push(t.clone()),
                    LlmEvent::ToolUse { id, name, input } => {
                        tool_calls.push((id.clone(), name.clone(), input.clone()));
                    }
                    LlmEvent::Stop => {}
                    LlmEvent::Error(e) => {
                        return Err(anyhow::anyhow!("LLM error: {e}"));
                    }
                }
            }

            // Build assistant message content
            let mut assistant_content: Vec<ContentBlock> = Vec::new();
            if !text_parts.is_empty() {
                assistant_content.push(ContentBlock::Text {
                    text: text_parts.join(""),
                });
            }
            for (id, name, input) in &tool_calls {
                assistant_content.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }

            self.messages.push(Message {
                role: "assistant".to_string(),
                content: assistant_content,
            });

            // Done only when the turn carries no tool calls. A turn with BOTH
            // text and tool_calls (common on Chat Completions — commentary
            // before the call) must keep looping so the tools actually run;
            // the text is already preserved in the assistant message above.
            if tool_calls.is_empty() {
                final_text = text_parts.join("");
                break;
            }

            // Execute tool calls and add results
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in &tool_calls {
                info!("executing tool: {name}");
                let result = self.execute_tool_call(name, input).await;
                match result {
                    Ok((output, is_error)) => {
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: output,
                            is_error,
                        });
                    }
                    Err(e) => {
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: format!("Error: {}", crate::mcp::concise_error_message(&e)),
                            is_error: Some(true),
                        });
                    }
                }
            }

            self.messages.push(Message {
                role: "user".to_string(),
                content: tool_results,
            });
        }

        if final_text.is_empty() {
            return Err(anyhow::anyhow!(
                "agent exceeded maximum tool loop iterations ({max_loops})"
            ));
        }

        Ok(final_text)
    }

    /// Drop oldest message pairs when context exceeds limit, preserving the
    /// first user message and maintaining strict user/assistant alternation.
    fn truncate_context(&mut self) {
        while self.messages.len() > MAX_CONTEXT_MESSAGES {
            // Remove the oldest assistant+user pair (indices 1 and 2), never
            // touching messages[0] (the first user message). The `min` clamp
            // means a trailing odd element still drains rather than panicking.
            let end = 3.min(self.messages.len());
            self.messages.drain(1..end);
        }
    }

    /// Route the `mcp` meta-tool to the MCP runtime when configured;
    /// everything else goes to the stateless `tools::execute_tool`. Keeping
    /// the routing here (rather than inside `tools.rs`) lets `tools.rs` stay
    /// stateless and free of MCP/feature plumbing.
    async fn execute_tool_call(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> Result<(String, Option<bool>)> {
        if name == mcp::MCP_TOOL_NAME {
            let Some(manager) = self.mcp_manager.as_ref() else {
                return Err(anyhow::anyhow!(
                    "mcp tool invoked but no McpRuntimeManager configured"
                ));
            };
            let action = mcp::meta_tool::Action::deserialize(input)
                .map_err(|e| anyhow::anyhow!("invalid mcp action payload: {e}"))?;
            let (value, is_error) = mcp::meta_tool::dispatch(manager, action).await?;
            return Ok((serde_json::to_string(&value)?, is_error));
        }
        tools::execute_tool(name, input, &self.working_dir)
            .await
            .map(|s| (s, None))
    }

    async fn call_llm(&self) -> Result<Vec<LlmEvent>> {
        self.provider
            .chat(&self.system_prompt, &self.messages, &self.tools)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Hand-written mock LLM provider for unit testing.
    struct MockLlmProvider {
        responses: Vec<Vec<LlmEvent>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockLlmProvider {
        fn new(responses: Vec<Vec<LlmEvent>>) -> Self {
            Self {
                responses,
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl LlmProvider for MockLlmProvider {
        fn model(&self) -> &str {
            "mock-model"
        }

        fn chat<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [Message],
            _tools: &'a [ToolDef],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>>
        {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let events = self.responses[idx].clone();
            Box::pin(async move { Ok(events) })
        }
    }

    #[tokio::test]
    async fn test_agent_simple_text_response() {
        let mock = MockLlmProvider::new(vec![vec![
            LlmEvent::Text("Hello!".to_string()),
            LlmEvent::Stop,
        ]]);

        let tmp = tempfile::TempDir::new().unwrap();
        let mut agent = Agent::new(mock, tmp.path().to_string_lossy().to_string());
        let result = agent.run("hi").await.unwrap();
        assert_eq!(result, "Hello!");
    }

    /// Stub provider with a fixed identity, for auth-policy tests.
    struct StubProvider {
        name: &'static str,
        oauth: bool,
    }
    impl LlmProvider for StubProvider {
        fn model(&self) -> &str {
            "stub"
        }
        fn is_oauth(&self) -> bool {
            self.oauth
        }
        fn provider_name(&self) -> &str {
            self.name
        }
        fn chat<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [Message],
            _tools: &'a [ToolDef],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<LlmEvent>>> + Send + 'a>>
        {
            Box::pin(async { Ok(vec![]) })
        }
    }

    #[test]
    fn anthropic_oauth_preference_is_sticky_across_provider_round_trips() {
        // Review round-3 F2: Anthropic-OAuth → xAI → Anthropic must remember
        // the OAuth policy; only an *active Anthropic* provider rewrites it.
        let tmp = tempfile::TempDir::new().unwrap();
        let mut agent = Agent::new_boxed(
            Box::new(StubProvider {
                name: "anthropic",
                oauth: true,
            }),
            tmp.path().to_string_lossy().to_string(),
            None,
        );
        assert!(agent.prefers_anthropic_oauth());

        // Switching away to xAI keeps the remembered Anthropic policy.
        agent.swap_provider(Box::new(StubProvider {
            name: "xai",
            oauth: true,
        }));
        assert!(agent.prefers_anthropic_oauth(), "policy lost on round trip");

        // Explicitly running Anthropic on an API key rewrites the policy…
        agent.swap_provider(Box::new(StubProvider {
            name: "anthropic",
            oauth: false,
        }));
        assert!(!agent.prefers_anthropic_oauth());

        // …and a session that never chose Anthropic OAuth never prefers it.
        let agent2 = Agent::new_boxed(
            Box::new(StubProvider {
                name: "xai",
                oauth: true,
            }),
            tmp.path().to_string_lossy().to_string(),
            None,
        );
        assert!(!agent2.prefers_anthropic_oauth());
    }

    #[tokio::test]
    async fn test_agent_mixed_text_and_tool_call_still_executes_tools() {
        // Review F1: a turn carrying BOTH commentary text and tool_calls (common
        // on Chat Completions) must keep looping so the tools actually run — not
        // end the turn with the commentary while the calls sit unexecuted in
        // history. The unknown tool name fails fast without touching the fs, so
        // this exercises the full round-trip as a unit test.
        let mock = MockLlmProvider::new(vec![
            vec![
                LlmEvent::Text("Let me check.".to_string()),
                LlmEvent::ToolUse {
                    id: "tu_1".to_string(),
                    name: "no_such_tool".to_string(),
                    input: serde_json::json!({}),
                },
            ],
            vec![LlmEvent::Text("Done.".to_string()), LlmEvent::Stop],
        ]);

        let tmp = tempfile::TempDir::new().unwrap();
        let mut agent = Agent::new(mock, tmp.path().to_string_lossy().to_string());
        let result = agent.run("go").await.unwrap();
        // The second LLM turn's text is the final reply — proof the loop continued.
        assert_eq!(result, "Done.");

        // user, assistant(text+tool_use), user(tool_result), assistant(text)
        assert_eq!(agent.messages.len(), 4);
        match &agent.messages[1].content[..] {
            [ContentBlock::Text { text }, ContentBlock::ToolUse { name, .. }] => {
                assert_eq!(text, "Let me check.");
                assert_eq!(name, "no_such_tool");
            }
            other => panic!("unexpected assistant content: {other:?}"),
        }
        match &agent.messages[2].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "tu_1");
                assert_eq!(*is_error, Some(true));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore] // Integration test: executes real file tools
    async fn test_agent_tool_call_then_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "file content here").unwrap();

        let mock = MockLlmProvider::new(vec![
            // First call: LLM requests to read a file
            vec![LlmEvent::ToolUse {
                id: "tu_1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({ "path": "test.txt" }),
            }],
            // Second call: LLM responds with text
            vec![
                LlmEvent::Text("The file contains: file content here".to_string()),
                LlmEvent::Stop,
            ],
        ]);

        let mut agent = Agent::new(mock, tmp.path().to_string_lossy().to_string());
        let result = agent.run("read test.txt").await.unwrap();
        assert_eq!(result, "The file contains: file content here");
    }

    #[tokio::test]
    #[ignore] // Integration test: executes real file tools
    async fn test_agent_tool_error_handling() {
        let tmp = tempfile::TempDir::new().unwrap();

        let mock = MockLlmProvider::new(vec![
            // First call: LLM requests to read a non-existent file
            vec![LlmEvent::ToolUse {
                id: "tu_1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({ "path": "nonexistent.txt" }),
            }],
            // Second call: LLM acknowledges the error
            vec![
                LlmEvent::Text("File not found.".to_string()),
                LlmEvent::Stop,
            ],
        ]);

        let mut agent = Agent::new(mock, tmp.path().to_string_lossy().to_string());
        let result = agent.run("read nonexistent.txt").await.unwrap();
        assert_eq!(result, "File not found.");

        // Verify the tool result was marked as error
        assert_eq!(agent.messages.len(), 4); // user, assistant(tool_use), user(tool_result), assistant(text)
        let tool_result_msg = &agent.messages[2];
        match &tool_result_msg.content[0] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert_eq!(*is_error, Some(true));
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn build_system_prompt_includes_mcp_catalogue_when_manager_provided() {
        // PR #959 F1 discovery slice: when an MCP manager is wired in, the
        // system prompt must surface the configured server catalogue so the
        // LLM knows `list_tools` is worth calling (the "fs disconnected, I
        // give up" failure mode the static const previously caused).
        use crate::mcp::config::McpConfig;
        let cfg: McpConfig = serde_json::from_str(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                    "linear": {
                        "type": "http",
                        "url": "https://mcp.linear.app/mcp",
                        "oauth": { "provider": "linear" }
                    }
                }
            }"#,
        )
        .unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);

        let tmp = tempfile::TempDir::new().unwrap();
        let prompt = Agent::build_system_prompt(&tmp.path().to_string_lossy(), Some(&mgr));

        assert!(
            prompt.contains("## MCP tool"),
            "missing MCP section:\n{prompt}"
        );
        assert!(
            prompt.contains("**fs** (stdio)"),
            "missing fs catalogue entry:\n{prompt}"
        );
        assert!(
            prompt.contains("requires `mcp login linear`"),
            "missing OAuth login hint:\n{prompt}"
        );
    }

    #[test]
    fn build_system_prompt_omits_mcp_section_when_no_manager() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prompt = Agent::build_system_prompt(&tmp.path().to_string_lossy(), None);
        assert!(
            !prompt.contains("## MCP tool"),
            "MCP section leaked into prompt without manager:\n{prompt}"
        );
    }

    #[tokio::test]
    #[ignore] // Integration test: executes real file tools
    async fn test_agent_multiple_tool_calls() {
        let tmp = tempfile::TempDir::new().unwrap();

        let mock = MockLlmProvider::new(vec![
            // First call: write a file
            vec![LlmEvent::ToolUse {
                id: "tu_1".to_string(),
                name: "write".to_string(),
                input: serde_json::json!({ "path": "out.txt", "content": "hello" }),
            }],
            // Second call: read it back
            vec![LlmEvent::ToolUse {
                id: "tu_2".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({ "path": "out.txt" }),
            }],
            // Third call: done
            vec![
                LlmEvent::Text("Done. File contains: hello".to_string()),
                LlmEvent::Stop,
            ],
        ]);

        let mut agent = Agent::new(mock, tmp.path().to_string_lossy().to_string());
        let result = agent
            .run("write hello to out.txt then read it")
            .await
            .unwrap();
        assert_eq!(result, "Done. File contains: hello");

        // Verify file was actually written
        let content = std::fs::read_to_string(tmp.path().join("out.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_max_tool_loops_default() {
        temp_env::with_var("OPENAB_AGENT_MAX_TOOL_LOOPS", None::<&str>, || {
            assert_eq!(max_tool_loops(), DEFAULT_MAX_TOOL_LOOPS);
        });
    }

    #[test]
    fn test_max_tool_loops_custom_value() {
        temp_env::with_var("OPENAB_AGENT_MAX_TOOL_LOOPS", Some("200"), || {
            assert_eq!(max_tool_loops(), 200);
        });
    }

    #[test]
    fn test_max_tool_loops_invalid_falls_back() {
        temp_env::with_var("OPENAB_AGENT_MAX_TOOL_LOOPS", Some("abc"), || {
            assert_eq!(max_tool_loops(), DEFAULT_MAX_TOOL_LOOPS);
        });
    }

    #[test]
    fn test_max_tool_loops_zero_clamps_to_one() {
        temp_env::with_var("OPENAB_AGENT_MAX_TOOL_LOOPS", Some("0"), || {
            assert_eq!(max_tool_loops(), 1);
        });
    }
}

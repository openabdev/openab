use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info};

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

const MAX_TOOL_LOOPS: usize = 50;
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
}

impl Agent {
    #[cfg(test)]
    pub fn new(provider: impl LlmProvider + 'static, working_dir: String) -> Self {
        let system_prompt = Self::build_system_prompt(&working_dir, None);
        Self {
            provider: Box::new(provider),
            messages: Vec::new(),
            working_dir: PathBuf::from(working_dir),
            system_prompt,
            tools: tools::tool_definitions(),
            mcp_manager: None,
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
        Self {
            provider,
            messages: Vec::new(),
            working_dir: PathBuf::from(working_dir),
            system_prompt,
            tools,
            mcp_manager,
        }
    }

    /// Replace the LLM provider while preserving conversation history.
    pub fn swap_provider(&mut self, provider: Box<dyn LlmProvider>) {
        self.provider = provider;
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

        for iteration in 0..MAX_TOOL_LOOPS {
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

            if tool_calls.is_empty() || !text_parts.is_empty() {
                // No tool calls — we're done
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
                "agent exceeded maximum tool loop iterations ({MAX_TOOL_LOOPS})"
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
}

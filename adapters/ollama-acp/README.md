# ollama-acp (Rust)

ACP (Agent Client Protocol) adapter for [Ollama](https://ollama.com) — enables openab to use local LLMs as the agent backend. Written in Rust for minimal overhead and no runtime dependencies.

## How it works

```
openab  ──stdin/stdout──▶  ollama-acp  ──HTTP──▶  Ollama (localhost:11434)
         (JSON-RPC 2.0)                           (OpenAI-compatible SSE)
```

## Quick start

```bash
# 1. Ensure Ollama is running
ollama serve

# 2. Pull a model
ollama pull gemma4:26b
# or: ollama pull qwen2.5:32b

# 3. Build the adapter
cd adapters/ollama-acp-rs
cargo build --release

# 4. Configure openab (config.toml)
# [agent]
# command = "ollama-acp"
# args = []
# working_dir = "/tmp"
# env = { OLLAMA_BASE_URL = "http://localhost:11434/v1", OLLAMA_MODEL = "gemma4:26b" }
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_BASE_URL` | `http://localhost:11434/v1` | Ollama OpenAI-compatible endpoint |
| `OLLAMA_MODEL` | `gemma4:26b` | Model to use for chat completions |
| `OLLAMA_API_KEY` | `ollama` | API key (Ollama ignores this, but required for API compat) |
| `OLLAMA_SYSTEM_PROMPT` | (auto-generated) | Custom system prompt for the assistant |

Also supports `LLM_BASE_URL`, `LLM_MODEL`, `LLM_API_KEY` as fallback variable names.

## Docker

```bash
docker build -f Dockerfile.ollama -t openab:ollama .
```

Pure Rust image — no Node.js runtime needed. ~30MB final image.

## Manual test

```bash
# Build
cargo build --release

# Run
./target/release/ollama-acp

# Paste these lines (one at a time):
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}
# Then use the sessionId from the response:
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"<id>","prompt":[{"type":"text","text":"Hello, say hi in one sentence"}]}}
```

## ACP protocol support

| Method | Status |
|--------|--------|
| `initialize` | Supported |
| `session/new` | Supported (multi-session) |
| `session/prompt` | Supported (streaming SSE) |

| Notification | Status |
|--------------|--------|
| `agent_message_chunk` | Emitted (streaming text) |
| `agent_thought_chunk` | Emitted (on prompt start) |
| `tool_call` | Emitted (LLM call tracking) |
| `tool_call_update` | Emitted (completion status) |

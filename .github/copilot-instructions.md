# GitHub Copilot Code Review Instructions

## Review Philosophy
- Only comment when you have HIGH CONFIDENCE (>80%) that an issue exists
- Be concise: one sentence per comment when possible
- Focus on actionable feedback, not observations
- Silence is preferred over noisy false positives

## Project Context
- **OpenAB**: A lightweight ACP (Agent Client Protocol) harness bridging Discord ↔ any ACP-compatible coding CLI over stdio JSON-RPC
- **Language**: Rust 2021 edition, single binary
- **Async runtime**: tokio (full features)
- **Discord**: serenity 0.12 (gateway + cache)
- **Error handling**: `anyhow::Result` everywhere, no `unwrap()` in production paths
- **Serialization**: serde + serde_json for ACP JSON-RPC, toml for config
- **Key modules**: `acp/connection.rs` (ACP stdio bridge), `acp/pool.rs` (session pool), `discord.rs` (Discord event handler), `config.rs` (TOML config), `usage.rs` (pluggable quota runners), `reactions.rs` (emoji reactions), `stt.rs` (speech-to-text)

## Priority Areas (Review These)

### Correctness
- Logic errors that could cause panics or incorrect behavior
- ACP JSON-RPC protocol violations (wrong method names, missing fields, incorrect response routing)
- Race conditions in async code (especially in the reader loop and session pool)
- Resource leaks (child processes not killed, channels not closed)
- Off-by-one in timeout calculations
- Incorrect error propagation — `unwrap()` in non-test code is always a bug

### Concurrency & Safety
- Multiple atomic fields updated independently — document if readers may see mixed snapshots
- `Mutex` held across `.await` points (potential deadlock)
- Session pool lock scope — `RwLock` held during I/O can stall all sessions
- Child process lifecycle — `kill_on_drop` must be set, zombie processes must not accumulate

### ACP Protocol
- `session/request_permission` must always get a response (auto-allow or forwarded)
- `session/update` notifications must not be consumed — forward to subscriber after capture
- `usage_update`, `available_commands_update`, `tool_call`, `agent_message_chunk` must be classified correctly
- Timeout values: session/new=120s, all other methods (including initialize)=30s

### Discord API
- Messages >2000 chars will be rejected — truncate or split
- Slash command registration is per-guild, max 100 per bot
- Autocomplete responses must return within 3s (no heavy I/O)
- Ephemeral messages for errors, regular messages for results

### Config & Deployment
- `config.toml` fields must have sensible defaults — missing `[usage]` section should not crash
- Environment variable expansion via `${VAR}` must handle missing vars gracefully
- Agent `env` map is passed to child processes — sensitive values should not be logged

## CI Pipeline (Do Not Flag These)
- `cargo fmt --check` — formatting is enforced by CI
- `cargo clippy --all-targets -- -D warnings` — lint warnings are enforced by CI
- `cargo test` — test failures are caught by CI

## Skip These (Low Value)
- Style/formatting — CI handles via rustfmt
- Clippy warnings — CI handles
- Minor naming suggestions unless truly confusing
- Suggestions to add comments for self-documenting code
- Logging level suggestions unless security-relevant
- Import ordering

## Response Format
1. State the problem (1 sentence)
2. Why it matters (1 sentence, only if not obvious)
3. Suggested fix (code snippet or specific action)

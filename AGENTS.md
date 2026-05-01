# OpenAB — Agent Guidelines

You are contributing to OpenAB, a lightweight Rust-based ACP harness bridging Discord, Slack, and webhook platforms to coding CLIs over stdio JSON-RPC.

## Architecture

```
src/
├── main.rs           # entrypoint, multi-adapter startup, graceful shutdown
├── adapter.rs        # ChatAdapter trait, AdapterRouter (platform-agnostic)
├── config.rs         # TOML config + ${ENV_VAR} expansion
├── discord.rs        # DiscordAdapter (serenity EventHandler)
├── slack.rs          # SlackAdapter (Socket Mode)
├── gateway.rs        # Custom Gateway WebSocket adapter
├── cron.rs           # Config-driven + usercron scheduler
├── media.rs          # Image resize/compress + STT download
├── format.rs         # Message splitting, thread name shortening
├── markdown.rs       # Discord markdown rendering
├── reactions.rs      # Emoji status controller (debounce, stall detection)
├── stt.rs            # Speech-to-text (Whisper API)
└── acp/
    ├── protocol.rs   # JSON-RPC types + ACP event classification
    ├── connection.rs # Spawn CLI, stdio JSON-RPC, env_clear whitelist
    └── pool.rs       # Session key → AcpConnection map + lifecycle
```

**Helm chart:** `charts/openab/` — Go templates, values.yaml, NOTES.txt
**Gateway:** `gateway/` — standalone Rust binary for Telegram/LINE/Teams
**Docs:** `docs/` — user-facing guides, ADRs, config reference

## Message Flow

```
Discord/Slack event
  → channel allowlist check
  → user allowlist check (bot messages bypass this)
  → bot message filter (off/mentions/all)
  → thread detection (thread_metadata = thread, NOT parent_id)
  → bot ownership check (bot_owns = thread creator matches bot user ID)
  → multibot detection (allowUserMessages mode)
  → handle_message → pool.get_or_create → ACP JSON-RPC
```

Understand this pipeline before modifying any adapter code.

## Rust Conventions

### Do
- `cargo fmt` + `cargo clippy -- -D warnings` before every commit
- Use `?` operator and `thiserror` enums for error handling
- Use `tracing` macros (`debug!`, `info!`, `warn!`, `error!`) — never `println!`
- Keep functions <50 lines, modules focused on one responsibility
- Test boundary cases: empty input, max limits, platform API constraints

### Do NOT
- `.unwrap()` / `.expect()` in production code — only in `#[cfg(test)]`
- `unsafe` without safety comment and explicit justification
- Blocking calls in async context — use `tokio::task::spawn_blocking`
- Global mutable state — pass via function args or `Arc<T>`
- Premature `.collect()` — keep iterators lazy

## Critical Rules (from past incidents)

### 1. Backward-Compatible Defaults

New config fields MUST default to the previous behavior. Never change what existing deployments experience without an explicit opt-in.

```rust
// WRONG: changes behavior for existing users
tool_display: String = "compact".to_string()

// RIGHT: preserves existing behavior
tool_display: String = "full".to_string()  // was always "full" before
```

### 2. Thread Detection

The definitive rules — do NOT reinvent this:
- A channel is a thread if `thread_metadata` is present (NOT `parent_id`)
- `bot_owns` = thread's `owner_id` matches the bot's user ID
- Use `ChannelType` enum, not structural heuristics
- Forum posts are threads with `parent_id` pointing to a forum channel

### 3. Security — Child Process Environment

Agent subprocesses start with `env_clear()`. Only `HOME`, `PATH`, and explicit `[agent].env` keys are passed. Never leak `DISCORD_BOT_TOKEN` or other OAB credentials to the agent.

### 4. Dockerfile Discipline

There are 7 Dockerfiles: `Dockerfile`, `Dockerfile.claude`, `Dockerfile.codex`, `Dockerfile.copilot`, `Dockerfile.cursor`, `Dockerfile.gemini`, `Dockerfile.opencode`.

**A change to one MUST be evaluated against ALL.** Common layers (base image, openab binary, tini) are shared — update all or explain why not.

### 5. Cross-Platform

Gate platform-specific code with `#[cfg(target_os = "...")]`. After adding Unix-only calls (libc, signals), verify with:
```bash
cargo check --target x86_64-pc-windows-gnu
```

### 6. Discord API Limits

- Select menu: max 25 options (truncate with count, don't crash)
- Message content: max 2000 chars (use `format.rs` splitting)
- Embeds: max 4096 chars description
- Rate limits: respect `Ratelimit` headers, use serenity's built-in ratelimiter

## Helm Chart Checklist

Before merging any chart change:

```bash
# Minimal values (default kiro agent)
helm template test charts/openab

# Full values (all agents enabled)
helm template test charts/openab -f charts/openab/ci/full-values.yaml

# Disabled agent path
helm template test charts/openab --set agents.kiro.enabled=false
```

Rules:
- Boolean fields: use `{{ if hasKey .Values "field" }}` not `{{ if .Values.field }}` (Go nil trap)
- Channel/user IDs: always `--set-string` (float64 precision loss)
- PVCs: set `"helm.sh/resource-policy": keep` — never delete user data on uninstall
- New values: add to `values.yaml` with comment + update `docs/config-reference.md`

## Breaking Changes

If your change alters existing behavior:

1. Add `!` to commit type: `feat(cron)!: description`
2. Include migration steps in PR body
3. Update relevant docs with migration callout
4. Ensure release note has ⚠️ Breaking Change section

## PR Standards

- One logical change per PR
- Commit message: `type(scope): description` — types: `feat`, `fix`, `docs`, `refactor`, `test`, `ci`, `build`
- Include tests for bug fixes (regression test proving the fix)
- Run full check before pushing:
  ```bash
  cargo fmt && cargo clippy -- -D warnings && cargo test
  ```
- Reference issue numbers: `Closes #123` or `Fixes #456`

## ADRs (Architecture Decision Records)

Read `docs/adr/` before implementing features in these areas:
- Cron scheduler → `docs/adr/basic-cronjob.md`
- Custom Gateway → `docs/adr/custom-gateway.md`
- LINE adapter → `docs/adr/line-adapter.md`

Write a new ADR if your feature touches >3 files or introduces a new subsystem.

# Handoff — feat/telegram-topics-idle-cleanup

## Current State

Branch: `feat/telegram-topics-idle-cleanup`  
Last commit: `fix: memory compaction — correct ACP text parsing and lock-free injection`  
Push pending: SSH key not available in this environment — run `git push origin feat/telegram-topics-idle-cleanup` manually.

## What's Done

1. ✅ Telegram adapter (`src/telegram.rs`) — full replacement of Discord
2. ✅ Forum topic threading (mimics Discord channels/threads)
3. ✅ Active/idle state + session eviction with user notification
4. ✅ Resume mechanism (`--resume` + `session/load`)
5. ✅ `!stop`, `!status`, `!restart` commands
6. ✅ `docs/session-management.md`
7. ✅ `docs/pr-telegram-topics-idle-cleanup.md` — ready to paste as PR description
8. ✅ Memory compaction on eviction — **WORKING** (Alice test passed 2026-04-05)

## Memory Compaction — Design & Status

**Alice test result: ✅ PASSED (2026-04-05)**

Flow:
1. Session goes idle → `cleanup_idle` fires compaction prompt (lock released before awaiting response)
2. ACP text parsed correctly via `classify_notification` → `AcpEvent::Text`
3. Summary stored in `SessionPool.summaries`
4. On next message to evicted thread → `get_or_create` sets `conn.pending_context`
5. `session_prompt` prepends context to first real user prompt (no extra round-trip, no lock contention)

Bugs fixed during development:
- **Deadlock #1**: `cleanup_idle` held `connections` write lock while awaiting compaction response → fixed by releasing lock before drain
- **Deadlock #2**: summary injection did a separate `session_prompt` round-trip inside `get_or_create` (still holding write lock) → fixed by storing as `pending_context` on `AcpConnection` and prepending at prompt time
- **Silent compaction**: summary drain loop parsed `msg.result.content[0].text` (wrong) → fixed to use `classify_notification` → `AcpEvent::Text` (correct ACP path: `params.update.content.text`)

## Pending

### Set Prod TTL
Update TTL constants in `src/telegram.rs` to prod values:
```rust
const CLEANUP_INTERVAL_SECS: u64 = 900;  // 15 min
const SESSION_TTL_SECS: u64 = 7200;      // 2 hr
```
Then commit:
```bash
git add src/telegram.rs
git commit -m "chore: set prod TTL 2hr/15min after Alice test"
git push origin feat/telegram-topics-idle-cleanup
```

### Telegram Bot Setup Doc
`docs/telegram-bot-howto.md` not yet written — mirrors `docs/discord-bot-howto.md` but for Telegram. Covers: BotFather setup, forum group config, `allowed_users`, `!` commands.

### Open PR
Use `docs/pr-telegram-topics-idle-cleanup.md` as the PR body when opening against `thepagent/agent-broker` main.

## TTL Reference

| Mode | CLEANUP_INTERVAL_SECS | SESSION_TTL_SECS |
|---|---|---|
| Current (test) | 60 | 120 |
| Production | 900 | 7200 |

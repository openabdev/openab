# ADR: Multi-Platform Adapter Architecture

- **Status:** Partially Implemented — Phase 1+3 landed via #259 (Slack adapter)
- **Date:** 2026-04-06
- **Author:** @chaodu-agent
- **Reviewers:** @dogzzdogzz, @antigenius0910
- **Tracking issues:** #86, #93

---

## 1. Context & Decision

Define a platform-agnostic adapter layer for agent-broker so it can serve Discord, Telegram, Slack, and future chat platforms through a single unified architecture. The ACP session pool and agent backend remain unchanged — only the "front door" becomes pluggable.

**Primary contract: simultaneous multi-platform (Contract B).** A single running instance can serve Discord + Slack (+ future adapters) concurrently, sharing one `SessionPool` with platform-namespaced session keys. This was validated by #259 which merged the Slack adapter with Discord + Slack running in one process.

> Contract A (pluggable single-platform per deployment) is a subset of B and works automatically — deploy with only one `[platform]` section in config.

## 2. Motivation

- agent-broker was originally hard-wired to Discord via `serenity`
- #86 proposes a Telegram adapter, #93 proposes Slack — both require similar abstractions
- Without a shared trait, each adapter will duplicate session routing, message splitting, reaction handling, and streaming logic
- A clean adapter boundary enables running multiple adapters simultaneously (e.g. Discord + Slack in one deployment, validated by #259)

---

## 3. Current Architecture (post-#259)

```
                    ┌─────────────────┐
                    │  Discord Users  │
                    └────────┬────────┘
                             │ Gateway WS (serenity)
                             ▼
                    ┌─────────────────┐
                    │ DiscordAdapter  │──┐
                    └─────────────────┘  │
                                         │  impl ChatAdapter
┌─────────────────┐                      │
│  Slack Users    │                      ▼
└────────┬────────┘             ┌─────────────────┐     ┌──────────────┐
         │ Socket Mode (WS)     │   AdapterRouter  │────►│ SessionPool  │
         ▼                      │                  │     │              │
┌─────────────────┐             │  - route message │     └──────┬───────┘
│  SlackAdapter   │────────────►│  - manage threads│            │ ACP stdio
└─────────────────┘             │  - stream edits  │     ┌──────▼───────┐
                                └─────────────────┘     │ AcpConnection │
                                         ▲              └───────────────┘
                                         │
                                ┌─────────────────┐
                                │ TelegramAdapter  │  (future — Phase 2)
                                └─────────────────┘
```

---

## 4. ChatAdapter Trait

The core abstraction. Each platform implements this trait.

```rust
#[async_trait]
pub trait ChatAdapter: Send + Sync + 'static {
    /// Platform name for logging and config ("discord", "telegram", "slack")
    fn platform(&self) -> &'static str;

    /// Maximum message length for this platform
    fn message_limit(&self) -> usize;

    /// Start listening for events. Blocks until shutdown.
    async fn run(&self, router: Arc<AdapterRouter>) -> Result<()>;

    /// Send a new message, returns platform-specific message ID
    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef>;

    /// Edit an existing message in-place
    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()>;

    /// Create a thread/topic from a message, returns thread channel ref
    async fn create_thread(&self, channel: &ChannelRef, trigger_msg: &MessageRef, title: &str) -> Result<ChannelRef>;

    /// Add a reaction/emoji to a message
    async fn add_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;

    /// Remove a reaction/emoji from a message
    async fn remove_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<()>;
}
```

**Design decisions:**
- `message_limit()` is on the trait (not hardcoded in router) so `AdapterRouter` can call `adapter.message_limit()` for `split_message` without matching on platform strings. (Feedback: @dogzzdogzz #1)
- Emoji handling: config stores Unicode emoji, each adapter converts internally to platform-specific format (e.g. Slack short names). (Feedback: @dogzzdogzz #6)

### Platform-Agnostic References

```rust
/// Identifies a channel/thread/topic across platforms
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct ChannelRef {
    pub platform: String,           // "discord", "telegram", "slack"
    pub channel_id: String,         // platform-native channel ID
    pub thread_id: Option<String>,  // thread within channel (Slack thread_ts, Telegram topic_id)
    pub parent_id: Option<String>,  // parent channel if thread-as-channel (Discord threads)
}

impl ChannelRef {
    /// Whether this ref points to a thread (either model)
    pub fn is_thread(&self) -> bool {
        self.thread_id.is_some() || self.parent_id.is_some()
    }
}
```

`ChannelRef` supports two threading models (Feedback: @dogzzdogzz #2):
- **Thread-as-reply-chain** (Slack, Telegram): same `channel_id`, identified by `thread_id`
- **Thread-as-child-channel** (Discord): separate `channel_id` with `parent_id` pointing to parent

```rust
/// Identifies a message across platforms
#[derive(Clone, Debug)]
pub struct MessageRef {
    pub platform: String,
    pub channel: ChannelRef,
    pub message_id: String,
}

/// Sender identity
#[derive(Clone, Debug, Serialize)]
pub struct SenderContext {
    pub schema: String,        // "openab.sender.v1"
    pub sender_id: String,
    pub sender_name: String,
    pub display_name: String,
    pub channel: String,       // platform name
    pub channel_id: String,
    pub is_bot: bool,
}
```

---

## 5. AdapterRouter

Shared logic extracted from `discord.rs` that is platform-independent:

```rust
pub struct AdapterRouter {
    pool: Arc<SessionPool>,
    reactions_config: ReactionsConfig,
}

impl AdapterRouter {
    /// Called by any adapter when a user message arrives.
    /// Handles: session creation, prompt injection, streaming, reactions.
    pub async fn handle_message(
        &self,
        adapter: Arc<dyn ChatAdapter>,
        channel: &ChannelRef,
        sender: &SenderContext,
        prompt: &str,
        trigger_msg: &MessageRef,
    ) -> Result<()>;
}
```

**Key design decisions:**
- `adapter` parameter is `Arc<dyn ChatAdapter>` (not `&dyn`) so it can be moved into `tokio::spawn` for the edit-streaming background task. (Feedback: @dogzzdogzz #5)
- Thread creation decision ("already in a thread?") lives in the router, using `ChannelRef::is_thread()`. Each adapter provides enough info in `ChannelRef` for the router to decide. (Feedback: @dogzzdogzz #3)
- `StatusReactionController` is decoupled from Serenity types — it receives `Arc<dyn ChatAdapter>` and calls `add_reaction()` / `remove_reaction()` through the trait. (Feedback: @dogzzdogzz #4)

The router owns:
- Session pool interaction (`get_or_create`, `with_connection`)
- Sender context injection (`<sender_context>` XML wrapping)
- Edit-streaming loop (1.5s interval, message splitting via `adapter.message_limit()`)
- Reaction state machine (queued → thinking → tool → done/error)
- Thread creation decision (new thread vs. existing thread)

Each adapter only needs to:
1. Listen for platform events
2. Determine if the message should be processed (allowed channels, @mention, thread check)
3. Call `router.handle_message()`

---

## 6. Platform Comparison

| Feature | Discord | Telegram | Slack |
|---------|---------|----------|-------|
| Connection | Gateway WebSocket (`serenity`) | Bot API polling / webhook (`teloxide`) | Socket Mode WebSocket / Events API |
| Threading model | Thread-as-child-channel | Forum topics (thread-as-reply) | Thread-as-reply-chain (`thread_ts`) |
| Message limit | 2000 chars | 4096 chars | 4000 chars (blocks: 3000) |
| Edit support | ✅ Full | ✅ Full | ✅ Full |
| Reactions | ✅ Unicode + custom emoji | ✅ Unicode emoji | ✅ Unicode + custom emoji (short names) |
| Trigger | `@mention` | `@mention` or any message (configurable) | `@mention` or app_mention event |
| Bot message filtering | `msg.author.bot` | `msg.from.is_bot` | `event.bot_id` present |
| Auth | Bot token | Bot token | Bot token + App token (Socket Mode) |

---

## 7. Config Design

```toml
# Enable one or more adapters. Multiple can run simultaneously.

[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["1234567890"]

[telegram]
bot_token = "${TELEGRAM_BOT_TOKEN}"
mode = "personal"                    # "personal" or "team" (see #86)
allowed_users = []                   # empty = deny all (secure by default, per #91)

[slack]
bot_token = "${SLACK_BOT_TOKEN}"
app_token = "${SLACK_APP_TOKEN}"     # for Socket Mode
allowed_channels = ["C1234567890"]

# Agent and pool config remain unchanged
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

[pool]
max_sessions = 10
session_ttl_hours = 24
```

### Startup Behavior

- agent-broker reads config and starts an adapter for each `[platform]` section present
- If no adapter section is configured → error and exit
- Each adapter runs as a separate tokio task, sharing the same `SessionPool`
- Session keys are namespaced: `discord:{thread_id}`, `telegram:{topic_id}`, `slack:{thread_ts}`

---

## 8. Message Size Handling

Each platform has different message limits. The `format::split_message` function accepts a configurable limit, sourced from `adapter.message_limit()`:

```rust
pub fn split_message(content: &str, max_len: usize) -> Vec<String>;
```

| Platform | Max chars | `message_limit()` |
|----------|-----------|-------------------|
| Discord  | 2000      | 1900 (safety margin) |
| Telegram | 4096      | 4000 |
| Slack    | 4000      | 3900 |

---

## 9. Reaction Mapping

The `StatusReactionController` uses `Arc<dyn ChatAdapter>` to call `add_reaction()` / `remove_reaction()`, fully decoupled from any platform SDK.

Config stores Unicode emoji. Each adapter converts internally:
- Discord: Unicode passthrough
- Slack: Unicode → short name mapping (e.g. `👀` → `eyes`)
- Telegram: Unicode passthrough

| Action | Discord | Telegram | Slack |
|--------|---------|----------|-------|
| Add reaction | `create_reaction()` | `set_message_reaction()` | `reactions.add` |
| Remove reaction | `delete_reaction()` | `set_message_reaction()` (empty) | `reactions.remove` |

---

## 10. Security Considerations

- **Secure by default** (#91): empty allowlist = deny all, for every platform
- **Bot message filtering**: each adapter must ignore messages from bots to prevent loops
- **Token isolation**: each platform's tokens are independent env vars
- **Session namespace isolation**: `discord:123` and `slack:123` are separate sessions even if IDs collide
- **Rate limiting**: platform-specific rate limits should be respected (Discord 5/5s, Slack 1/s per channel, Telegram 30/s)

---

## 11. Implementation Phases

| Phase | Scope | Status | Notes |
|-------|-------|--------|-------|
| **Phase 1** | Extract `ChatAdapter` trait + `AdapterRouter`, refactor Discord to implement trait | ✅ Merged (#259) | Pure refactor + Slack adapter landed together |
| **Phase 2** | Telegram adapter (`teloxide`), personal + team modes | Not started | Depends on #86 |
| **Phase 3** | Slack adapter (Socket Mode), channel threading | ✅ Merged (#259) | Includes simultaneous Discord + Slack |
| **Phase 4** | Per-adapter session soft limits, streaming timeout | Not started | See Known Limitations |
| **Phase 5** | Platform-specific features: Slack blocks, Telegram inline keyboards, Discord embeds | Not started | Text-only for v1; extend via `PlatformExt` traits later |

---

## 12. Kubernetes / Helm Considerations

- Single image supports all adapters (all compiled in)
- Helm `values.yaml` gains `telegram.*` and `slack.*` sections
- Adapter selection is config-driven, not build-time
- PVC storage structure unchanged — auth tokens are per-agent, not per-platform

---

## 13. Testing Strategy

(Feedback: @dogzzdogzz #8)

### MockAdapter

Define a `MockAdapter` (in-memory, no network) that implements `ChatAdapter`:

```rust
pub struct MockAdapter {
    pub sent_messages: Arc<Mutex<Vec<(ChannelRef, String)>>>,
    pub edited_messages: Arc<Mutex<Vec<(MessageRef, String)>>>,
    pub reactions: Arc<Mutex<Vec<(MessageRef, String)>>>,
}

impl ChatAdapter for MockAdapter {
    fn platform(&self) -> &'static str { "mock" }
    fn message_limit(&self) -> usize { 2000 }
    // ... record calls for assertion
}
```

### Test coverage

| Layer | What to test | How |
|-------|-------------|-----|
| `AdapterRouter` | Session routing, streaming, reactions, thread creation | Unit tests with `MockAdapter` |
| `ChatAdapter` impls | Platform-specific API calls, emoji mapping, error handling | Integration tests per adapter (can mock HTTP) |
| `ChannelRef` | `is_thread()` logic for both threading models | Unit tests |
| End-to-end | Full message flow through router → pool → ACP | Integration test with `MockAdapter` + real `SessionPool` |

### Phase 1 acceptance criteria (validated by #259)

- ✅ Existing Discord behavior passes through the new trait boundary
- ✅ `MockAdapter` can drive `AdapterRouter` in tests
- ✅ No behavior change for Discord-only deployments

---

## 14. Resolved Questions

(Feedback: @dogzzdogzz #7, @antigenius0910)

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | Shared vs separate `SessionPool` | **Shared** | Already namespaced by platform prefix; separate pools add complexity with no isolation benefit |
| 2 | Platform prefix in session keys | **Yes, always prefix** | IDs from different platforms can collide; prefix cost is negligible. Format: `{platform}:{thread_id}` |
| 3 | Global vs per-adapter `max_sessions` | **Global hard cap + per-adapter soft limit** | Prevents one noisy platform from starving others. Global cap in `[pool]`, soft limits in each `[platform]` section |
| 4 | Rich messages in v1 | **Text-only** | Ship the abstraction first; rich messages are additive and platform-divergent |
| 5 | Platform-specific features | **Defer to Phase 5** | Keep `ChatAdapter` minimal for v1; extend via `PlatformExt` traits or feature flags later |
| 6 | Primary contract: A (single) vs B (simultaneous) | **B — simultaneous multi-platform** | Validated by #259. Contract A is a subset that works automatically |

---

## 15. Known Limitations

(Feedback: @antigenius0910)

### Resolved: SessionPool lock contention

@antigenius0910 raised that `SessionPool::with_connection()` held a write lock for the full prompt duration, which would serialize all sessions across platforms under Contract B.

**Status: Resolved.** The current implementation uses read lock + `Arc` clone + drop lock, then locks only the individual connection:

```rust
pub async fn with_connection<F, R>(&self, thread_id: &str, f: F) -> Result<R> {
    let conn = {
        let state = self.state.read().await;  // read lock, not write
        state.active.get(thread_id).cloned()   // clone Arc
            .ok_or_else(|| anyhow!("no connection"))?
    };  // state lock dropped here
    let mut conn = conn.lock().await;  // lock individual connection
    f(&mut conn).await
}
```

Multiple adapters' sessions can now execute concurrently without blocking each other.

### Open: No streaming timeout

A hung ACP session (e.g. agent process stuck, infinite tool loop) will hold its connection `Mutex` indefinitely. Under Contract B, this doesn't block other sessions (each has its own `Mutex`), but it does permanently consume one session slot from the pool.

**Suggested fix (follow-up issue):** Add a configurable timeout to `with_connection`:

```rust
let mut conn = tokio::time::timeout(
    Duration::from_secs(config.streaming_timeout_secs),
    conn.lock()
).await.map_err(|_| anyhow!("session timeout"))??;
```

This would allow the pool to reclaim hung sessions and is especially important as the number of simultaneous adapters grows.

---

_This RFC was updated on 2026-04-21 to integrate review feedback from @dogzzdogzz (8 architectural points) and @antigenius0910 (Contract A vs B question), and to reflect the merge of #259 (Slack adapter implementing Phase 1+3)._

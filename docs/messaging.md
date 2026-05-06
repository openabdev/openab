# Messaging Model

This document explains the five messaging patterns in OpenAB, each building on the previous one:

0. **Human → Bot in DM** — Private 1:1 conversations (opt-in).
1. **Human → Bot in Channel** — How a conversation starts via @mention.
2. **Human → Bot in Thread** — How follow-up messages work without @mention.
3. **Human → Multiple Bots in Thread** — How multi-bot threads behave and how to control them.
4. **Bot → Bot in Thread** — How bots can talk to each other and how to prevent loops.

---

## 0. Human → Bot in DM

Users can interact with the bot privately via direct message. DMs are **opt-in** — disabled by default to prevent unexpected resource usage.

When `allow_dm = true`, a DM is treated as an **implicit @mention** (mirrors Slack behavior). No thread is created — the bot replies directly in the DM channel.

```
User DMs BotA: help me with X
  → BotA replies in DM (no thread, no @mention needed)
```

### Key behaviors

- **`allowed_users` still enforced** — DMs are not a backdoor past user allowlists.
- **Bot turn tracking applies** — `max_bot_turns` prevents loops in DM conversations.
- **Session pool shared** — Each DM user consumes one session slot (`discord:{dm_channel_id}`). Existing TTL cleanup and eviction apply.
- **Discord only** — Slack natively supports DMs without extra config. This setting applies to the Discord adapter only.

### Relevant config

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allow_dm` | bool | `false` | `true` = respond to Discord DMs; `false` = ignore DMs. |
| `allowed_users` | string[] | `[]` | User IDs allowed to interact. Still enforced in DMs. |

### Example config.toml

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allow_dm = true                      # opt-in to DM support
allowed_users = ["9876543210"]       # restrict who can DM the bot
```

---

## 1. Human → Bot in Channel

Bots **never** respond to regular channel messages. A conversation starts only when a human explicitly @mentions a bot.

When you @mention a bot in a channel:

1. **Ack** — The bot reacts to your message with an emoji (e.g., 👀) to confirm receipt.
2. **Thread creation** — OAB automatically creates a dedicated thread.
3. **Response** — The bot responds inside that thread.

```
User in #general: @BotA help me with X
  → BotA reacts 👀
  → OAB creates thread "help me with X"
  → BotA replies in thread
```

### Relevant config

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allow_all_channels` | bool \| omit | auto-detect | `true` = all channels; `false` = only `allowed_channels`. |
| `allowed_channels` | string[] | `[]` | Channel IDs where bots can be activated. |
| `allow_all_users` | bool \| omit | auto-detect | `true` = any user; `false` = only `allowed_users`. |
| `allowed_users` | string[] | `[]` | User IDs allowed to interact with bots. |
| `reactions.enabled` | bool | `true` | Enable/disable emoji reaction feedback. |

### Example config.toml

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["1234567890"]    # restrict to specific channels
allowed_users = ["9876543210"]       # restrict to specific users
# By default, all channels and all users are allowed when these lists are empty.
# Reactions are enabled by default with 👀 for ack.
```

---

## 2. Human → Bot in Thread

Once a thread is created, **no @mention is needed** for follow-up messages. All your messages in the thread are automatically routed to the bot.

```
User in thread: can you also do Y?
  → BotA replies (no @mention required)
```

### Relevant config

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allow_user_messages` | string | `"involved"` | Controls when bots respond without @mention. See Layer 3 for all modes. |

In this single-bot scenario, the default `"involved"` means the bot responds to all messages in threads it has participated in.

### Example config.toml

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
# allow_user_messages defaults to "involved":
# bot responds to all messages in threads it has participated in,
# no @mention needed for follow-ups.
```

---

## 3. Human → Multiple Bots in Thread

You can bring additional bots into a thread by @mentioning them. Once a bot responds, it becomes **involved** in the thread.

How involved bots behave on subsequent messages is controlled by `allow_user_messages`:

| Mode | Behavior |
|------|----------|
| `involved` (default) | All involved bots respond to every message — no @mention required. |
| `mentions` | Always require an explicit @mention, even in threads. |
| `multibot-mentions` | Like `involved`, but once a second bot has posted in the thread, you must @mention the bot(s) you want to respond. |

```
# allow_user_messages = "involved" (default)
User in thread: @BotB what do you think?
  → BotB replies, now "involved"
User in thread: any other ideas?
  → Both BotA and BotB reply

# allow_user_messages = "multibot-mentions"
User in thread: any other ideas?
  → No bot replies (need explicit @mention)
User in thread: @BotA any other ideas?
  → Only BotA replies
```

### Relevant config

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allow_user_messages` | string | `"involved"` | `"involved"` — reply without @mention in participated threads. `"mentions"` — always require @mention. `"multibot-mentions"` — require @mention once 2+ bots are in the thread. |

> **Note:** This is a **global setting** — it cannot be changed per thread. Configure it in `config.toml` or via `values.yaml` for Helm.

### Example config.toml

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
# Default is "involved" — all involved bots respond without @mention.
# Use "multibot-mentions" for precise control in multi-bot threads.
allow_user_messages = "multibot-mentions"
```

---

## 4. Bot → Bot in Thread

Bots can talk to each other within a thread. By default this is disabled.

| Mode | Behavior |
|------|----------|
| `off` (default) | Bots ignore all messages from other bots. |
| `mentions` | A bot only processes messages from other bots that explicitly @mention it. |
| `all` | A bot processes all bot messages in threads it's involved in. |

```
# allow_bot_messages = "mentions"
BotA in thread: @BotB can you review this?
  → BotB processes and replies

# allow_bot_messages = "all"
BotA in thread: here's my analysis
  → BotB automatically responds (no @mention needed)
  → Continues until max_bot_turns is reached
```

### Relevant config

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `allow_bot_messages` | string | `"off"` | `"off"` — ignore bot messages. `"mentions"` — only process bot messages that @mention this bot. `"all"` — process all bot messages (capped by `max_bot_turns`). |
| `trusted_bot_ids` | string[] | `[]` | Whitelist of bot IDs. When non-empty, only these bots pass the bot gate. Empty = any bot (mode permitting). Ignored when `allow_bot_messages = "off"`. |
| `max_bot_turns` | u32 | `20` | Max consecutive bot turns per thread before throttling. A human message resets the counter. |

> **Safety:** When `allow_bot_messages = "all"`, a separate hardcoded cap of 10 consecutive bot turns applies regardless of `max_bot_turns`.

### Example config.toml

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
# Default is "off" — bots ignore all messages from other bots.
# Set to "mentions" to allow bot-to-bot via explicit @mentions.
allow_bot_messages = "mentions"
# Default is empty — any bot is allowed (mode permitting).
# Set trusted_bot_ids to restrict which bots can interact.
trusted_bot_ids = ["1111111111", "2222222222"]
# Default is 20. Cap consecutive bot turns to prevent runaway loops.
max_bot_turns = 10
```

### Helm chart

Same keys are settable from chart values under `agents.<name>.discord` and
`agents.<name>.slack` using camelCase (Helm convention):

```yaml
agents:
  claude:
    discord:
      allowBotMessages: "mentions"
      trustedBotIds: ["1111111111", "2222222222"]
      maxBotTurns: 50
    slack:
      allowBotMessages: "mentions"
      trustedBotIds: ["U1111111111", "U2222222222"]
      maxBotTurns: 50
```

When `maxBotTurns` is omitted from values, the Rust default of 20
applies. The hard cap of 100 is compiled-in
(`HARD_BOT_TURN_LIMIT` in `src/bot_turns.rs`) and is not chart-tunable.

---

## Quick Reference

```
Layer 0 — Human → Bot (DM)
  Config: allow_dm, allowed_users
  DM to bot                     →  Bot replies directly (no thread, no @mention)

Layer 1 — Human → Bot (Channel)
  Config: allowed_channels, allowed_users, reactions
  @BotA in channel              →  OAB creates thread, BotA responds

Layer 2 — Human → Bot (Thread)
  Config: allow_user_messages
  Message in thread             →  BotA responds (no @mention needed)

Layer 3 — Human → Multiple Bots (Thread)
  Config: allow_user_messages
    "involved"                  →  All involved bots respond
    "mentions"                  →  Only @mentioned bot responds
    "multibot-mentions"         →  Must @mention once 2+ bots are involved

Layer 4 — Bot → Bot (Thread)
  Config: allow_bot_messages, trusted_bot_ids, max_bot_turns
    "off"                       →  Bots ignore other bots
    "mentions"                  →  Only if explicitly @mentioned by a bot
    "all"                       →  All involved bots respond (capped)
```

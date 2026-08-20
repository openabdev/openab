# Slash Commands

OpenAB registers Discord slash commands for session control and agent management. Most work in both guild threads and DMs — the exception is `/auth`, which is **DM-only** for security (see [`/auth`](#auth) below).

The first six session/configuration commands share one Core semantic service.
Discord keeps native ephemeral interactions; Gateway platforms receive
boundary-valid plain-text commands after their normal trust gates. See the
[proposed Teams command decision](adr/teams-text-command-parity.md) and its
[live-validation status](msteams-live-validation.md).

## Commands

| Command | Description | Requires active session? |
|---------|-------------|--------------------------|
| `/models` | Select the AI model via dropdown menu | Yes |
| `/agents` | Select the agent mode via dropdown menu | Yes |
| `/cancel` | Cancel the current in-flight operation | Yes |
| `/cancel-all` | Cancel the operation and clear every buffered lane in the thread | Active session or buffered messages |
| `/reset` | Reset the conversation session (clear history, start fresh) | Active session or buffered messages |
| `/usage` | Show backend account usage and billing information | Yes; Kiro backend |
| `/auth` | Authenticate the backend agent via device flow (**DM-only**) | No |
| `/remind` | Set a one-shot delayed reminder to mention users/roles | No |
| `/export-thread` | Export thread/DM as `.txt` (default: last 100 messages) | No |

Discord native interaction responses are **ephemeral** — only the user who invoked the command sees the reply. Gateway text-command responses are ordinary platform messages and are not inherently private.

## Platform Support

| Platform | Supported | Notes |
|----------|-----------|-------|
| Discord (guild threads) | ✅ | Native commands registered globally; ephemeral responses |
| Discord (DMs) | ✅ | Native commands registered globally; may take time to propagate |
| Teams | ✅ | Ordinary authenticated message text; all six commands plus `/model …` and `/agent …`. `/usage` requires typed Personal scope. |
| Other Gateway platforms | ⚠️ Partial | Shared text commands are intercepted, but `/usage` fails closed because those surfaces do not yet prove private response visibility. |
| Slack | ❌ | Slack blocks third-party slash commands in threads; see [slack.md](slack.md#slash-commands-are-not-supported-on-slack) |

## How They Work

### /models and /agents

These read `configOptions` from the ACP `initialize` / `session/new` response and present them as a Discord Select Menu.

When the user picks an option, OpenAB sends `session/set_config_option` to the ACP backend.

**Agent support varies:**

| Agent | `/models` | `/agents` |
|-------|-----------|-----------|
| openab-agent | ✅ Returns available models via `configOptions` in `session/new` response | ❌ |
| kiro-cli | ✅ Returns available models via `models` fallback | ✅ Returns modes (`kiro_default`, `kiro_planner`) via `modes` fallback |
| claude-code | ❌ No `configOptions` emitted | ❌ |
| codex | ❌ | ❌ |
| gemini | ❌ | ❌ |
| cursor-agent | ❌ (tracking: #493) | ❌ |
| copilot | ❌ (tracking: #496) | ❌ |

If the agent doesn't expose options, the user sees: `⚠️ No model options available. Start a conversation first by @mentioning the bot.`

> **Backward compatibility:** `openab-agent` returns `configOptions` in the `session/new` response (alongside `sessionId`). ACP clients that only read `sessionId` will continue to work — `configOptions` is additive. Clients that support `/models` should read `configOptions[].options` to populate the model picker. Each model option includes a `provider` field (`"anthropic"` or `"openai"`) for routing.

<!-- Separate adjacent blockquotes for markdownlint MD028. -->

> **Note:** Discord Select Menus are limited to 25 items per page. OpenAB paginates larger option sets and places the current selection on the first page.

### /cancel

Sends a `session/cancel` JSON-RPC notification to the ACP backend. This aborts in-flight LLM requests and tool calls immediately — no need to wait for the current response to finish.

### /cancel-all

Sends the same cancellation as `/cancel` and removes every buffered dispatcher
lane for the logical thread. Unlike `/reset`, it retains the session history.

### /reset

Cancels any in-flight operation, clears every buffered dispatcher lane, then removes the session from the pool. The ACP process terminates once the last reference is released. The next message in the thread or DM will automatically create a fresh session.

This is equivalent to the `sessions close` + `sessions new` pattern used by [OpenClaw ACPX](https://github.com/openclaw/acpx).

**What gets cleared:**

- Conversation history
- ACP process and connection
- Suspended session state (no resume after reset)

**What is preserved:**

- Bot identity and system prompt (re-applied on next session creation)
- Config settings in `config.toml`

### /usage

Queries the existing Kiro ACP usage extension for a live session. Discord
returns an ephemeral plan and usage breakdown. Teams permits the command only
when authenticated typed scope proves a Personal DM; group/channel and old
untyped events are rejected before the backend query because their replies are
not proven private.

### /export-thread

Fetches the current Discord thread or DM history and returns a `.txt` file as an ephemeral follow-up. The transcript includes message timestamps, author names and IDs, message text, and attachment URLs.

**Optional parameters** (mutually exclusive — use at most one):

| Parameter | Type | Description |
|-----------|------|-------------|
| `limit` | Integer | Export only the most recent N messages (1–5000) |
| `since` | String | Export messages after this message ID (right-click → Copy Message ID) |
| `days` | Integer | Export messages from the last N days (1–365). Rolling N×24h window from now. |
| `all` | Boolean | Export all messages (up to 5000) |

If no parameter is provided, the **last 100 messages** are exported.

**Examples:**

```
/export-thread                              → last 100 messages (default)
/export-thread limit:500                    → most recent 500 messages
/export-thread since:1503744866100842698    → messages after this specific message
/export-thread days:3                       → messages from the last 3 days (rolling 72h)
/export-thread all:true                     → export all (cap 5000)
```

**Constraints:**

- Only works in allowed Discord threads or enabled DMs.
- Specifying more than one filter returns an error.
- Very large exports may be truncated to fit Discord's attachment size limit.

## Passing Agent CLI Commands via Discord @mention

In Discord ordinary messages, you can pass agent-native CLI commands directly after an @mention:

```
@MyBot /compact
@MyBot /clear
@MyBot /model claude-sonnet-4
```

These Discord prompts are forwarded as-is to the ACP session. Any command the underlying CLI supports in its interactive mode works here. This is the recommended workaround for agents that don't expose `configOptions`.

Gateway text surfaces reserve the boundary-valid `/model …` and `/agent …`
compatibility forms for the broker command service; unknown slash commands such
as `/compact` still pass through to the ACP backend.

## /remind

Set a one-shot delayed reminder that mentions users or roles in the channel after a specified delay.

**Syntax:**

```
/remind targets:<@user @role ...> message:<text> delay:<duration>
```

**Parameters:**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `targets` | Yes | Space-separated @mentions (users and/or roles) |
| `message` | Yes | Reminder text |
| `delay` | Yes | Duration before firing: `1m` to `30d` (supports `m`, `h`, `d` and combinations like `1h30m`) |

**Constraints:**

- Only humans can use `/remind` (bots are rejected)
- Minimum delay: 1 minute
- Maximum delay: 30 days
- Maximum message length: 1800 characters
- Maximum 5 active reminders per user
- Maximum 10 mention targets per reminder (use a @role for larger groups)
- `@everyone` and `@here` in messages are automatically neutralized (will not trigger mass mentions)
- One-shot only (fires once, then removed)
- Reminders persist across bot restarts (stored in `$HOME/.openab/reminders.json`)

**Examples:**

```
/remind targets:@Alice @Bob message:Review PR #42 delay:2h
/remind targets:@Reviewers message:Stand-up time delay:30m
/remind targets:@Charlie message:Check deployment delay:1d
```

**When fired, the bot posts:**

```
⏰ Reminder from @sender:
"Review PR #42"
cc @Alice @Bob
```

## /auth

Trigger the backend agent's device-flow authentication. OAB executes the command defined in `OPENAB_AGENT_AUTH_COMMAND`, captures the device code URL from stdout/stderr, and relays it to the user as an ephemeral Discord message.

**Flow:**

1. User runs `/auth`
2. OAB executes `$OPENAB_AGENT_AUTH_COMMAND` (e.g. `codex login --device-auth`)
3. OAB captures the device URL + code from the command's output
4. OAB sends an ephemeral reply with the URL and code
5. User opens the URL in their browser, enters the code
6. The auth command exits successfully → OAB confirms "✅ Authentication successful!"

**Requirements:**

- `OPENAB_AGENT_AUTH_COMMAND` environment variable must be set
- The auth command must use OAuth device flow (print URL + code to stdout, then block until authorized)
- No interactive stdin input required (headless-compatible)
- Must be invoked in a **DM** with the bot (rejected in guild channels/threads for security)

**Timeout:** 14 minutes. If the user doesn't authorize within that window, the process is killed and the user is prompted to run `/auth` again. (Reduced from 15min to leave headroom for Discord's interaction token TTL.)

**Behavior notes:**

- Only users in the `allowed_users` list can invoke `/auth`
- Bot users are rejected — `/auth` is for human operators only
- A 30-second URL-collection window waits for the auth command to print its URL. Slow-starting CLIs that take longer may show "no output".
- Only one `/auth` flow can run at a time (single-flight). A second concurrent invocation is rejected with "already in progress".

**Error cases:**

- `OPENAB_AGENT_AUTH_COMMAND` not set → immediate error message
- Invoked by a bot user → rejected
- Invoked outside a DM (in a guild channel/thread) → rejected for security
- Auth command fails to start → error message
- Auth command exits **before** printing a login URL (within the 30s window) → warning that no URL was produced, with a retry prompt
- Auth command exits with non-zero → failure message with exit code
- Timeout → process killed, retry prompt

## Maintaining This Guide

- **Trigger:** command parsing, admission order, response privacy, ACP backend
  support, or manifest command-list changes.
- **Action:** update the command and platform matrices, then run:

  ```bash
  cargo test -p openab-core commands
  cargo test --manifest-path crates/platform-schema/Cargo.toml teams_manifest
  ```

- **Why:** the Core command service and manifest fixture are authoritative;
  platform discovery does not override trust or privacy gates.

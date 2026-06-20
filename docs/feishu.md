# Feishu / Lark

Connect OpenAB to Feishu (China) or Lark (international) so users can chat with an AI agent in DMs or group chats.

## Prerequisites

1. Create a Feishu/Lark app at [open.feishu.cn](https://open.feishu.cn/) or [open.larksuite.com](https://open.larksuite.com/).
2. Enable the **Bot** capability.
3. In **Event Subscriptions**, select **Long Connection** (WebSocket) mode.
4. Add the `im.message.receive_v1` event.
5. Grant the following permission scopes:
   - `im:message` — receive messages
   - `im:message:send_as_bot` — send messages as bot
   - `contact:user.base:readonly` — resolve sender display names (recommended; without it, senders show as `ou_xxx`)
6. Copy the **App ID** and **App Secret** from **Credentials & Basic Info**.

## Quick Start (Helm)

```yaml
agents:
  kiro:
    gateway:
      enabled: true
      url: "ws://openab-kiro-gateway:8080/ws"
      platform: "feishu"
      botUsername: "ou_YOUR_BOT_OPEN_ID"  # bot's open_id for @mention gating
      feishu:
        appId: "cli_xxx"
        appSecret: "secret_xxx"
        domain: "feishu"           # "feishu" or "lark"
        connectionMode: "websocket" # recommended
```

```bash
helm upgrade --install openab charts/openab \
  --set-literal agents.kiro.gateway.feishu.appSecret="your-secret"
```

## Connection Modes

### WebSocket (default, recommended)

The gateway connects outbound to Feishu — no public URL, TLS, or Ingress required.

Set `connectionMode: "websocket"` (default).

### Webhook (fallback)

Use when outbound WebSocket is blocked by your network.

```yaml
feishu:
  connectionMode: "webhook"
  webhookPath: "/webhook/feishu"
  verificationToken: "your-token"
  encryptKey: "your-key"
```

Then configure the webhook URL in Feishu Open Platform → Event Subscriptions → Request URL:
```
https://your-gateway-host/webhook/feishu
```

## Configuration Reference

| Helm Value | Env Var | Default | Description |
|---|---|---|---|
| `feishu.appId` | `FEISHU_APP_ID` | — | App ID (required) |
| `feishu.appSecret` | `FEISHU_APP_SECRET` | — | App Secret (required, stored in K8s Secret) |
| `feishu.domain` | `FEISHU_DOMAIN` | `feishu` | `feishu` (China) or `lark` (international) |
| `feishu.connectionMode` | `FEISHU_CONNECTION_MODE` | `websocket` | `websocket` or `webhook` |
| `feishu.webhookPath` | `FEISHU_WEBHOOK_PATH` | `/webhook/feishu` | Webhook endpoint path |
| `feishu.verificationToken` | `FEISHU_VERIFICATION_TOKEN` | — | Webhook verification token (stored in K8s Secret) |
| `feishu.encryptKey` | `FEISHU_ENCRYPT_KEY` | — | Webhook encrypt key (stored in K8s Secret) |
| `feishu.allowedGroups` | `FEISHU_ALLOWED_GROUPS` | — | Comma-separated chat_id allowlist |
| `feishu.allowedUsers` | `FEISHU_ALLOWED_USERS` | — | Comma-separated open_id allowlist |
| `feishu.requireMention` | `FEISHU_REQUIRE_MENTION` | `true` | Require @mention in groups |
| — | `FEISHU_DEDUPE_TTL_SECS` | `300` | Event deduplication cache TTL (seconds) |
| — | `FEISHU_MESSAGE_LIMIT` | `4000` | Max message length before auto-splitting (bytes) |
| — | `FEISHU_ALLOW_BOTS` | `off` | Bot message handling: `off` / `mentions` / `all` |
| — | `FEISHU_TRUSTED_BOT_IDS` | — | Comma-separated open_id list of known bots |
| — | `FEISHU_MAX_BOT_TURNS` | `20` | Max consecutive bot replies per channel before suppression |
| — | `FEISHU_SESSION_TTL_HOURS` | `24` | How long the bot remembers thread participation (hours). After expiry, @mention is required again. |
| — | `FEISHU_ALLOW_USER_MESSAGES` | `multibot-mentions` | Thread response mode: `multibot-mentions` / `involved` / `mentions`. See below. |
| `gateway.botUsername` | — | — | Set to bot's `open_id` for @mention gating |
| `gateway.streaming` | — | `false` | Enable streaming (typewriter) mode |
| `cardStreaming.mode` | `FEISHU_CARD_STREAMING_MODE` | `auto` | Card streaming mode: `auto` (short→post, long/code/table→card), `card` (always card), `post` (disable — kill-switch) |
| `cardStreaming.fallbackToPost` | `FEISHU_CARD_FALLBACK_TO_POST` | `true` | Fall back to a post message if a card API call fails |
| `cardStreaming.promoteBytes` | `FEISHU_CARD_PROMOTE_BYTES` | `4000` | Byte threshold for auto-promoting a plain-text reply to a card |
| `cardStreaming.idleFinalizeMs` | `FEISHU_CARD_IDLE_FINALIZE_MS` | `3000` | Idle window (ms) before a streaming card is finalized |

## @mention Gating

In group chats, the bot only responds when @mentioned (default). To find your bot's `open_id`:

1. Start the gateway — it logs the bot identity on startup:
   ```
   feishu bot identity resolved bot_open_id=ou_xxx
   ```
2. Set `gateway.botUsername` to this value.

To disable mention gating: `feishu.requireMention: false`.

### Thread Participation (Involved Mode)

Once the bot replies in a thread (topic), it remembers that thread and responds to subsequent messages **without requiring @mention** — similar to Discord's `allow_user_messages: "involved"` mode.

- Only applies to threads (messages with `root_id`). Main channel messages always require @mention.
- Participation is stored in memory. Gateway restart clears the cache; users need to @mention once to re-engage.
- TTL controlled by `FEISHU_SESSION_TTL_HOURS` (default 24h). After expiry, @mention is required again.

### Multi-Bot Threads (multibot-mentions Mode)

When `FEISHU_ALLOW_USER_MESSAGES=multibot-mentions`, the bot detects when another bot is @mentioned in a participated thread and reverts to requiring @mention — preventing all bots from responding simultaneously.

| Mode | Behavior |
|------|----------|
| `multibot-mentions` (default) | Like `involved`, but requires @mention once another bot has posted in the thread. |
| `involved` | Bot responds in participated threads without @mention. All participated bots respond. |
| `mentions` | Always require @mention, even in participated threads. |

**Multi-bot detection** (how the gateway identifies "another bot"):

1. If `FEISHU_TRUSTED_BOT_IDS` is set → exact match against configured IDs
2. If only `FEISHU_ALLOWED_USERS` is set → any @mention that is not self and not in allowed_users is inferred as another bot (recommended, zero-config)
3. If neither is set → no multibot detection

Note: Detection only triggers in threads where the bot has already participated. This prevents premature marking of threads the bot hasn't joined.

## Security Notes

- `appSecret`, `verificationToken`, and `encryptKey` are stored in a Kubernetes Secret, not in ConfigMap.
- In webhook mode, always set both `verificationToken` and `encryptKey` for production.
- The gateway enforces a 1 MB body size limit and per-IP rate limiting (120 req/60s) on the webhook endpoint.

## Slash Commands

The gateway intercepts slash commands before they reach the agent:

| Command | Action |
|---------|--------|
| `/reset` | Clears the conversation session. |
| `/cancel` | Sends a cancel signal to the running agent. |
| `/model list` | Numbered list of available models with ✅ current selection. |
| `/model set <name or number>` | Switch model by exact name or list number. |
| `/models` | Alias of `/model list`. |
| `/agent list` | Numbered list of available agents with ✅ current selection. |
| `/agent set <name or number>` | Switch agent by exact name or list number. |
| `/agents` | Alias of `/agent list`. |

`/model` and `/agent` commands require an active session — send a message first to start one. These work in both DMs and group chats, across all gateway platforms.

## Rich Text (Post) Messages

Agent replies are sent as Feishu **post** (rich text) messages instead of plain text. This enables:

- Fenced code blocks with syntax highlighting
- Clickable hyperlinks
- Proper line breaks and paragraph structure

Inline Markdown formatting (`**bold**`, `*italic*`, `` `code` ``, `~~strike~~`) is stripped to plain text because Feishu's post format does not support inline styles.

## Image & File Attachments

The gateway downloads and forwards image and text file attachments to the AI agent, matching Discord's attachment handling.

**Supported message types:**

| Feishu msg_type | Handling |
|-----------------|----------|
| `text` | Text extracted, forwarded as prompt |
| `image` | Image downloaded, resized (max 1200px), JPEG compressed, stored to `~/.openab/media/inbound/<uuid>` → `ContentBlock::Image` |
| `file` | Text files only (`.txt`, `.py`, `.rs`, `.md`, `.json`, etc., max 512KB). Non-text files (`.pdf`, `.zip`, etc.) are silently ignored. |
| `audio` | Voice message downloaded (opus/ogg, max 25MB), stored to filesystem, forwarded to core. If `[stt]` is enabled, core transcribes via Whisper API and injects `[Voice message transcript]: ...` into the prompt. If STT is disabled or fails, the message is silently skipped. |
| `post` | Rich text: text nodes extracted as prompt, `img` nodes downloaded as image attachments. This is the format Feishu uses when @mention + paste image in a group. |

**Group chat limitation:** Feishu does not allow @mention and image upload in the same message. However, @mention + paste (Ctrl+V) an image works — Feishu sends this as a `post` message containing both the mention and the image. Direct image upload (via the attachment button) cannot include @mention, so the bot will not respond in groups.

**Processing pipeline:** Gateway downloads media using `GET /im/v1/messages/{message_id}/resources/{key}?type=image` with `tenant_access_token`, resizes to max 1200px, compresses to JPEG (quality 75), and stores to `~/.openab/media/inbound/<uuid>`. The file path is passed in `GatewayEvent.content.attachments[].path`. OAB core reads the file directly from disk and converts to `ContentBlock::Image` or `ContentBlock::Text` for the AI agent.

## Streaming (Typewriter)

Agent replies stream incrementally — a placeholder message appears immediately, then updates every ~1.5 seconds as the agent generates content. This matches Discord's streaming behavior.

To enable streaming, set `streaming = true` in the gateway config:

```toml
[gateway]
url = "ws://127.0.0.1:8080/ws"
platform = "feishu"
streaming = true
```

The gateway platform must support message editing (Feishu/Lark do). Platforms that don't support editing should leave `streaming = false` (default).

## Card Streaming

By default (`FEISHU_CARD_STREAMING_MODE=auto`), streaming replies render as
**interactive CardKit cards** when the content warrants it. Cards have no
20-edit cap (errcode 230072) and render markdown — including **tables** and code
blocks — natively, which a `post` message cannot.

| Mode | Behavior |
|---|---|
| `auto` (default) | Short replies stay as a `post` (native reply UI); long replies, or any reply containing a code fence or a markdown table, promote to a card. |
| `card` | Every reply is sent as a card from the first message. |
| `post` | Card streaming disabled — post-only behavior (the kill-switch). |

Notes:

- **Auto promotion is one-way**: a reply starts as a post and, once promoted,
  stays a card. Promotion deletes the post placeholder (shown as "message
  recalled" in Feishu) and re-sends as a card. In `card` mode the first reply is
  a card from the start, so there is no placeholder and no recall.
- **Finalize**: after ~`FEISHU_CARD_IDLE_FINALIZE_MS` ms with no further edits,
  the card is rebuilt as a static card so the typewriter cursor settles and the
  markdown re-renders cleanly.
- **Fallback**: if a card API call fails and `FEISHU_CARD_FALLBACK_TO_POST` is
  `true` (default), the gateway falls back to the post path (with the edit-cap
  recovery), so a reply is never lost.
- **Tables wrapped in a code fence**: agents sometimes wrap a markdown table in a
  bare ``` fence for monospace alignment in environments that don't render
  tables. On the card path the gateway unwraps a fence whose body is exactly one
  table so it renders as a native table.

## Thread (Topic) Replies

When a user replies to a bot message in a group chat, Feishu creates a thread (topic). The bot replies within the same thread, and each thread gets its own independent session.

To start a threaded conversation: reply to any bot message in a group chat (long-press or hover → Reply). The bot's response will appear in the same thread. Subsequent messages in the thread still require @mention (same as group chat).

**How it works:** Feishu reply events include a `root_id` (the original message that started the thread). The gateway uses this as `thread_id` for session isolation. Replies are sent via `POST /im/v1/messages/{root_id}/reply` to stay in the thread.

**Limitation:** Messages sent directly in the Feishu thread panel (not via the "Reply" action) do not include `root_id` and will be treated as regular group messages. Use the "Reply" action to ensure thread context is preserved.

Streaming (typewriter) mode works in threads — edits target the same message regardless of thread context.

## Agent-Controlled Reply-To

Agents can reply to a specific message using the `[[reply_to:message_id]]` output directive (see [docs/output-directives.md](output-directives.md)). The gateway sends the reply via Feishu's native Reply API, showing a quote reference in the UI.

```
Agent output:
  [[reply_to:om_xxx]]
  This is my reply to that specific message.
```

**How agents get message IDs:** Every incoming message includes `message_id` in the `SenderContext` injected into the agent prompt. Agents can store and reference these IDs to reply to specific messages.

**Fallback:** If the specified message ID is invalid or the Reply API fails, the gateway automatically falls back to a plain send (no quote).

**Use case:** In multi-bot threads, each bot can reply to a different message, creating clear visual conversation threads within a Feishu thread.

## Bot-to-Bot Collaboration (Gateway-Side Only)

The gateway adapter includes bot identification and filtering scaffolding (`AllowBots` enum, `FEISHU_TRUSTED_BOT_IDS`, `FEISHU_MAX_BOT_TURNS` with human-reset safety valve), matching Discord's `allow_bot_messages` design.

Bot identification requires explicit configuration via `FEISHU_TRUSTED_BOT_IDS` because Feishu marks other bots as `sender_type="user"` — they cannot be identified from the event alone.

> **Not yet functional.** Two blockers remain:
> 1. **Feishu platform limitation:** Feishu does not deliver bot-sent messages to other bots' WebSocket connections.
> 2. **OAB core limitation:** `src/gateway.rs` unconditionally drops `is_bot` events before they reach the router. When blocker 1 is lifted, the core guard must become adapter-aware to let gateway-filtered bot events through.

## Troubleshooting

| Problem | Fix |
|---|---|
| Bot doesn't respond | Check `FEISHU_APP_ID`/`FEISHU_APP_SECRET` are correct. Check gateway logs for token errors. |
| Bot doesn't respond in groups | Ensure bot is @mentioned, or set `requireMention: false`. Check `botUsername` matches bot's `open_id`. |
| WebSocket keeps reconnecting | Check event subscription is set to **Long Connection** mode. Check app is published and approved. |
| Webhook verification fails | Ensure `verificationToken` and `encryptKey` match Feishu app config. |
| Messages from Lark (international) | Set `domain: "lark"` to use `open.larksuite.com` API endpoints. |

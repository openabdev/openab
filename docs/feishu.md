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
| `gateway.botUsername` | — | — | Set to bot's `open_id` for @mention gating |
| `gateway.streaming` | — | `false` | Enable streaming (typewriter) mode |

## @mention Gating

In group chats, the bot only responds when @mentioned (default). To find your bot's `open_id`:

1. Start the gateway — it logs the bot identity on startup:
   ```
   feishu bot identity resolved bot_open_id=ou_xxx
   ```
2. Set `gateway.botUsername` to this value.

To disable mention gating: `feishu.requireMention: false`.

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

## Thread (Topic) Replies

When a user replies to a bot message in a group chat, Feishu creates a thread (topic). The bot replies within the same thread, and each thread gets its own independent session.

To start a threaded conversation: reply to any bot message in a group chat (long-press or hover → Reply). The bot's response will appear in the same thread. Subsequent messages in the thread still require @mention (same as group chat).

**How it works:** Feishu reply events include a `root_id` (the original message that started the thread). The gateway uses this as `thread_id` for session isolation. Replies are sent via `POST /im/v1/messages/{root_id}/reply` to stay in the thread.

**Limitation:** Messages sent directly in the Feishu thread panel (not via the "Reply" action) do not include `root_id` and will be treated as regular group messages. Use the "Reply" action to ensure thread context is preserved.

Streaming (typewriter) mode works in threads — edits target the same message regardless of thread context.

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

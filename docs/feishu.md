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
| `gateway.botUsername` | — | — | Set to bot's `open_id` for @mention gating |

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

## Troubleshooting

| Problem | Fix |
|---|---|
| Bot doesn't respond | Check `FEISHU_APP_ID`/`FEISHU_APP_SECRET` are correct. Check gateway logs for token errors. |
| Bot doesn't respond in groups | Ensure bot is @mentioned, or set `requireMention: false`. Check `botUsername` matches bot's `open_id`. |
| WebSocket keeps reconnecting | Check event subscription is set to **Long Connection** mode. Check app is published and approved. |
| Webhook verification fails | Ensure `verificationToken` and `encryptKey` match Feishu app config. |
| Messages from Lark (international) | Set `domain: "lark"` to use `open.larksuite.com` API endpoints. |

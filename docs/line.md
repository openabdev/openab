# LINE Setup

Connect a LINE bot to OpenAB via the Custom Gateway.

```
LINE ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                   (OAB connects out)
```

## Prerequisites

- A running OAB instance (with kiro-cli or any ACP agent authenticated)
- The Custom Gateway deployed ([gateway/README.md](../gateway/README.md))
- A LINE Official Account with Messaging API enabled

## 1. Create a LINE Official Account

1. Go to [LINE Official Account Manager](https://manager.line.biz) and create a new account
2. After creation, go to **Settings** → **Messaging API** → **Enable Messaging API**
3. Select or create a Provider, then confirm

The channel now appears in the [LINE Developers Console](https://developers.line.biz).

## 2. Get Credentials

In the LINE Developers Console, open your channel:

- **Basic settings** tab → **Channel secret** → copy (→ `LINE_CHANNEL_SECRET`)
- **Messaging API** tab → scroll to bottom → **Channel access token** → Issue → copy (→ `LINE_CHANNEL_ACCESS_TOKEN`)

## 3. Configure the Gateway

Add the LINE env vars to your gateway deployment:

```bash
# Docker
docker run -d --name openab-gateway \
  -e TELEGRAM_BOT_TOKEN="..." \
  -e LINE_CHANNEL_SECRET="your-channel-secret" \
  -e LINE_CHANNEL_ACCESS_TOKEN="your-channel-access-token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:0.3.0

# Kubernetes
kubectl set env deployment/openab-gateway \
  LINE_CHANNEL_SECRET="your-channel-secret" \
  LINE_CHANNEL_ACCESS_TOKEN="your-channel-access-token"
```

## 4. Set the Webhook URL

In the LINE Developers Console → **Messaging API** tab:

1. **Webhook URL** → Edit → enter: `https://gw.yourdomain.com/webhook/line`
2. **Use webhook** → ON
3. **Auto-reply messages** → OFF (prevents LINE's default auto-reply from interfering)
4. Click **Verify** to test the connection

## 5. Configure OAB

```toml
[gateway]
url = "ws://openab-gateway:8080/ws"
platform = "line"
# allowed_users = ["U1234567890abcdef"]   # restrict to specific LINE user IDs
# allowed_channels = ["C1234567890abcdef"] # restrict to specific chat/group IDs

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

> **Tip:** To find a LINE user ID, check the gateway logs — the sender ID is logged for each incoming message. By default all users and channels are allowed. Setting `allowed_users` or `allowed_channels` automatically restricts access to only those listed.

## 6. Add the Bot as Friend

In the LINE Developers Console → **Messaging API** tab → scan the QR code with your LINE app, or search for the bot by its LINE ID.

## Features

### Supported

- **1:1 chat** — send a message to the bot, get an AI agent response
- **Group chat** — add the bot to a group, it responds to all messages
- **Webhook signature validation** — HMAC-SHA256 via `LINE_CHANNEL_SECRET`

### Not Supported (LINE API limitations)

- **Threads** — LINE has no thread/topic concept. All messages in a chat share one agent session.
- **Reactions** — LINE Bot API does not support message reactions.
- **@mention gating** — LINE does not expose mention entities. In groups, the bot responds to all messages. To limit this, use a dedicated group for the bot.
- **Markdown rendering** — LINE uses its own text formatting. Agent replies are sent as plain text.

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `LINE_CHANNEL_SECRET` | Yes | Channel secret for webhook signature validation |
| `LINE_CHANNEL_ACCESS_TOKEN` | Yes | Channel access token for Push Message API |

## Troubleshooting

**Bot doesn't respond:**
- Verify webhook URL is correct and shows ✅ in LINE Developers Console
- Check **Use webhook** is ON and **Auto-reply messages** is OFF
- Check gateway logs: `kubectl logs -l app=openab-gateway`

**"Invalid signature" in gateway logs:**
- Verify `LINE_CHANNEL_SECRET` matches the value in LINE Developers Console
- Make sure you're using the Channel secret (not the Channel access token)

**Webhook verify fails:**
- Ensure the gateway is reachable at the webhook URL
- Check `curl https://your-gateway-host/health` returns `ok`

## References

- [LINE Messaging API Documentation](https://developers.line.biz/en/docs/messaging-api/)
- [LINE Developers Console](https://developers.line.biz)
- [ADR: Custom Gateway](../docs/adr/custom-gateway.md)
- [ADR: LINE Adapter](../docs/adr/line-adapter.md)

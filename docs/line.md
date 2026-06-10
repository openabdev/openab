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
- **Group chat** — add the bot to a group; it responds only when @-mentioned (see @mention gating below)
- **Inbound images** — user-sent LINE images are downloaded through the LINE Content API and forwarded to OpenAB as image attachments
- **Webhook signature validation** — HMAC-SHA256 via `LINE_CHANNEL_SECRET`

> **Implementation tradeoff:** OpenAB now acknowledges LINE webhooks before image download/processing so slow attachment work is less likely to trigger webhook redelivery. The follow-up image download and event emission happen asynchronously, which keeps the request path short but also means a crash after the HTTP 200 can still lose that in-flight work. This PR intentionally keeps scope small and does not add a separate background-task durability or duplicate-suppression layer on top of early-ack.
> Because image processing now happens after the ACK, an earlier image webhook can also reach OpenAB after a later text webhook from the same chat if the image path is slower.
> OpenAB now also caps how many LINE payloads can enter that post-ACK path concurrently; once the cap is full, new webhooks wait for capacity instead of creating unbounded background backlog.
> If a LINE-hosted image cannot be downloaded or decoded, OpenAB logs and skips that image event rather than synthesizing a fake text prompt.

### Not Supported (LINE API limitations)

- **Threads** — LINE has no thread/topic concept. All messages in a chat share one agent session.
- **Reactions** — LINE Bot API does not support message reactions.
- **@mention gating** — Supported (zero-config). In group/room chats the gateway only forwards messages where the bot is explicitly @-mentioned (LINE's native `mentionees[].isSelf` signal). 1:1 DMs are always forwarded. No env var is needed.
  - *Limitation — non-text messages*: LINE only attaches mention data to text messages. Images, videos, stickers, files, and location messages in groups are silently dropped because they cannot carry an @-mention.
  - *Limitation — `@All`*: A group-wide `@All` mention does **not** trigger the bot; only a direct `@BotName` mention does.
  - *Breaking change*: This gating is always active. Deployments that previously relied on the bot responding to all group messages will need to @-mention the bot after upgrading.
- **Markdown rendering** — LINE uses its own text formatting. Agent replies are sent as plain text.
- **External-content images** — LINE image messages backed by `contentProvider.type = "external"` are not downloaded yet.

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `LINE_CHANNEL_SECRET` | Yes | Channel secret for webhook signature validation |
| `LINE_CHANNEL_ACCESS_TOKEN` | Yes | Channel access token for Reply/Push Message API and LINE-hosted image downloads |

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

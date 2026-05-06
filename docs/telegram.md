# Telegram Setup

Connect a Telegram bot to OpenAB via the Custom Gateway.

```
Telegram ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                       (OAB connects out)
```

## Prerequisites

- A running OAB instance (with kiro-cli or any ACP agent authenticated)
- Docker or a Kubernetes cluster
- A Telegram bot token (from [@BotFather](https://t.me/BotFather))

## 1. Create a Telegram Bot

1. Open [@BotFather](https://t.me/BotFather) in Telegram
2. Send `/newbot`, follow the prompts
3. Copy the bot token (e.g. `123456:ABC-DEF...`)
4. Optional: send `/setprivacy` → `Disable` so the bot can see all group messages (required for @mention gating in groups)

## 2. Run the Gateway

### Docker

```bash
docker run -d --name openab-gateway \
  -e TELEGRAM_BOT_TOKEN="your-bot-token" \
  -e TELEGRAM_SECRET_TOKEN="your-webhook-secret" \
  -e GATEWAY_WS_TOKEN="your-ws-auth-token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:0.1.0
```

### Kubernetes

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: openab-gateway
spec:
  replicas: 1
  selector:
    matchLabels:
      app: openab-gateway
  template:
    metadata:
      labels:
        app: openab-gateway
    spec:
      containers:
        - name: gateway
          image: ghcr.io/openabdev/openab-gateway:0.1.0
          ports:
            - containerPort: 8080
          env:
            - name: TELEGRAM_BOT_TOKEN
              valueFrom:
                secretKeyRef:
                  name: openab-gateway
                  key: telegram-bot-token
            - name: TELEGRAM_SECRET_TOKEN
              valueFrom:
                secretKeyRef:
                  name: openab-gateway
                  key: telegram-secret-token
            - name: GATEWAY_WS_TOKEN
              valueFrom:
                secretKeyRef:
                  name: openab-gateway
                  key: ws-token
            - name: GATEWAY_LISTEN
              value: "0.0.0.0:8080"
---
apiVersion: v1
kind: Service
metadata:
  name: openab-gateway
spec:
  selector:
    app: openab-gateway
  ports:
    - port: 8080
      targetPort: 8080
```

## 3. Configure OAB

Add a `[gateway]` section to your OAB `config.toml`:

```toml
[gateway]
url = "ws://openab-gateway:8080/ws"
platform = "telegram"
token = "${GATEWAY_WS_TOKEN}"
bot_username = "your_bot_username"
# allowed_users = ["123456789"]          # restrict to specific Telegram user IDs
# allowed_channels = ["-1001234567890"]  # restrict to specific chat/group IDs

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

| Key | Required | Description |
|---|---|---|
| `url` | Yes | WebSocket URL of the gateway |
| `platform` | No | Session key namespace (default: `telegram`) |
| `token` | No | Shared WS auth token (recommended) |
| `bot_username` | No | Bot username for @mention gating in groups |
| `allowed_users` | No | Restrict to listed user IDs (empty = allow all) |
| `allowed_channels` | No | Restrict to listed chat IDs (empty = allow all) |

## 4. Set the Telegram Webhook

The gateway needs a public HTTPS URL for Telegram to send updates to.

### Option A: Cloudflare Tunnel (quickest for dev/testing)

```bash
cloudflared tunnel --url http://localhost:8080
# Copy the https://xxx.trycloudflare.com URL
```

### Option B: Reverse proxy (production)

Use nginx, Caddy, or a cloud load balancer with TLS termination pointing to the gateway's `:8080`.

### Register the webhook

```bash
export BOT_TOKEN="your-bot-token"
export WEBHOOK_URL="https://your-gateway-host"
export SECRET="your-webhook-secret"

curl "https://api.telegram.org/bot${BOT_TOKEN}/setWebhook?url=${WEBHOOK_URL}/webhook/telegram&secret_token=${SECRET}"
```

Verify:

```bash
curl "https://api.telegram.org/bot${BOT_TOKEN}/getWebhookInfo"
```

## 5. Bot Permissions for Supergroups

For forum topic creation (thread isolation like Discord):

1. Open the supergroup → Settings → Administrators
2. Find the bot → Edit
3. Enable **Manage Topics**

Without this permission, the bot replies in the main chat instead of creating topics.

## Features

### @mention gating

In groups and supergroups, the bot only responds when @mentioned:

```
@your_bot explain VPC peering    ← triggers agent
explain VPC peering              ← ignored in groups
```

DMs and replies within forum topics always trigger the agent (no @mention needed).

### Emoji reactions

The bot shows status reactions on your message as the agent works:

| Stage | Emoji |
|---|---|
| Queued | 👀 |
| Thinking | 🤔 |
| Tool use | 🔥 (general), 👨‍💻 (coding), ⚡ (web) |
| Done | 👍 |
| Error | 😱 |

### Forum topics

In supergroups with topics enabled, each new conversation auto-creates a forum topic (like Discord threads). Follow-up messages in the same topic reuse the same agent session.

### Markdown rendering

Agent replies are rendered with Telegram Markdown: **bold**, `code`, and code blocks work. Headers (`##`) and tables render as plain text (Telegram limitation).

## Environment Variables (Gateway)

| Variable | Required | Default | Description |
|---|---|---|---|
| `TELEGRAM_BOT_TOKEN` | Yes | — | Bot API token from @BotFather |
| `TELEGRAM_SECRET_TOKEN` | No | — | Webhook signature validation |
| `GATEWAY_WS_TOKEN` | No | — | WebSocket auth token |
| `GATEWAY_LISTEN` | No | `0.0.0.0:8080` | Listen address |
| `TELEGRAM_WEBHOOK_PATH` | No | `/webhook/telegram` | Webhook endpoint path |

## Troubleshooting

**Bot doesn't respond in groups:**
- Check bot privacy mode: `/setprivacy` → `Disable` in @BotFather
- Verify `bot_username` in OAB config matches the bot's actual username
- Check the bot is @mentioned in the message

**"not enough rights to create a topic":**
- Give the bot **Manage Topics** permission in supergroup admin settings

**Webhook returns 502/530:**
- Check the Cloudflare Tunnel or reverse proxy is running
- Verify `curl http://localhost:8080/health` returns `ok`

**Agent spawns but immediately closes:**
- Run `kubectl exec -it deployment/openab-telegram -- kiro-cli login --use-device-flow`
- Ensure auth is persisted on a PVC, not an emptyDir

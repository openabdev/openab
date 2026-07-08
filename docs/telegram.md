# Telegram Setup

Connect a Telegram bot to OpenAB.

## Deployment Modes

| Mode | Description | When to use |
|------|-------------|-------------|
| **Unified** (recommended) | Single OAB binary with embedded webhook server | New deployments, ECS, k8s, Zeabur |
| **Standalone Gateway** | Separate gateway process, OAB connects via WebSocket | Legacy deployments, custom routing |

## Unified Mode (Recommended)

The OAB binary embeds the Telegram adapter directly. No separate gateway container needed.

```
Telegram ──POST──▶ OAB (:8080/webhook/telegram) ──▶ Agent (stdio)
```

### Prerequisites

- OAB image with unified features compiled in (default since v0.9.0-beta.4)
- A Telegram bot token (from [@BotFather](https://t.me/BotFather))
- A public HTTPS URL for the webhook

### Configuration

Set environment variables:

| Variable | Required | Description |
|----------|----------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | Bot API token from @BotFather |
| `TELEGRAM_SECRET_TOKEN` | No | Webhook signature validation |
| `TELEGRAM_BOT_USERNAME` | No | Bot username for @mention gating |
| `TELEGRAM_RICH_MESSAGES` | No | `true` (default) for rich formatting |
| `TELEGRAM_STREAMING` | No | follows `TELEGRAM_RICH_MESSAGES` | Stream replies via rich message drafts. Defaults to `true` when rich messages are enabled, `false` otherwise. Set explicitly to override |
| `TELEGRAM_ALLOWED_USERS` | No | Comma-separated Telegram user IDs allowed to interact (empty = deny all) |
| `TELEGRAM_ALLOW_ALL_USERS` | No | `true`/`false` — defaults to `false` (deny-all). Set `true` to allow everyone. |
| `GATEWAY_LISTEN` | No | Listen address (default: `0.0.0.0:8080`) |

OAB config (`config.toml`):

**Minimal** — bot token via env var, API key passed to agent:

```toml
[telegram]
bot_token = "${TELEGRAM_BOT_TOKEN}"
allow_all_users = true

[agent]
env = { KIRO_API_KEY = "${KIRO_API_KEY}" }
```

**Recommended** — with access control, tuned pool, and streaming:

```toml
[telegram]
bot_token = "${TELEGRAM_BOT_TOKEN}"
allowed_users = ["176096071"]

[agent]
env = { KIRO_API_KEY = "${KIRO_API_KEY}" }

[pool]
max_sessions = 3
session_ttl_hours = 1

[reactions]
tool_display = "compact"
```

Table rendering is automatically disabled for Telegram (tables pass through as native markdown for Rich Messages). To force code-block wrapping, set explicitly:

```toml
[markdown]
tables = "code"
```

Streaming is enabled by default when Rich Messages are active — replies are streamed live via `sendRichMessageDraft` with rich formatting, then finalized with `sendRichMessage`. If `TELEGRAM_RICH_MESSAGES=false`, streaming is also disabled by default. To override, set `TELEGRAM_STREAMING=true` or `TELEGRAM_STREAMING=false` explicitly.

No `[gateway]` section needed — the unified adapter activates automatically when `TELEGRAM_BOT_TOKEN` is set, or when the `[telegram]` section is configured in `config.toml`.

### First-class `[telegram]` config (optional)

Instead of (or in addition to) the `TELEGRAM_*` env vars, you can configure Telegram as a first-class section in `config.toml` — symmetric with `[discord]` / `[slack]`:

**Minimal required** — only `bot_token` is needed to activate the adapter:

```toml
[telegram]
bot_token = "${TELEGRAM_BOT_TOKEN}"    # or use aws-sm:// secret ref (see below)
```

**Full example** with all available fields:

```toml
[telegram]
bot_token           = "${TELEGRAM_BOT_TOKEN}"   # ${} env expansion supported
secret_token        = "${TELEGRAM_SECRET_TOKEN}" # webhook signature validation
trusted_source_only = true     # reject requests outside Telegram's IP subnets
rich_messages       = true     # sendRichMessage rendering (default true)
streaming           = true     # override; defaults to follow rich_messages
webhook_path        = "/webhook/telegram"
allowed_users       = ["12345678"]  # restrict to specific Telegram user IDs
# allow_all_users   = true           # set true to allow everyone (default: false)
```

**Precedence (per field):** `[telegram]` value (with `${}` expansion) → `TELEGRAM_*` env var → built-in default. This is config-authoritative and matches `[discord]`/`[slack]`. Any field you omit falls back to its env var, so existing env-only deployments keep working unchanged.

| `[telegram]` field | Env fallback | Default |
|--------------------|--------------|---------|
| `bot_token` | `TELEGRAM_BOT_TOKEN` | — |
| `secret_token` | `TELEGRAM_SECRET_TOKEN` | — |
| `trusted_source_only` | `TELEGRAM_TRUSTED_SOURCE_ONLY` | `false` |
| `rich_messages` | `TELEGRAM_RICH_MESSAGES` | `true` |
| `streaming` | `TELEGRAM_STREAMING` | follows `rich_messages` |
| `webhook_path` | `TELEGRAM_WEBHOOK_PATH` | `/webhook/telegram` |
| `allowed_users` | `TELEGRAM_ALLOWED_USERS` (comma-separated) | `[]` (deny all if empty) |
| `allow_all_users` | `TELEGRAM_ALLOW_ALL_USERS` | `false` (deny-all) |

> **Tip**: You can run a pure config-only deployment — no `TELEGRAM_*` env vars needed. Just set `bot_token = "your-token"` directly in `[telegram]` and the adapter will activate from config alone.

> **Security hardening**: For production deployments, we highly recommend using `aws-sm://` secret references instead of hardcoding tokens in `config.toml`. This keeps secrets out of version control and enables rotation and audit:
>
> ```toml
> [secrets.refs]
> tg_token  = "aws-sm://openab/prod#telegram_bot_token"
> tg_secret = "aws-sm://openab/prod#telegram_secret_token"
>
> [telegram]
> bot_token    = "${secrets.tg_token}"
> secret_token = "${secrets.tg_secret}"
> ```
>
> See [secrets-management.md](secrets-management.md) for full documentation.


### User Access Control

Restrict which Telegram users can interact with the bot using `allowed_users`:

```toml
[telegram]
bot_token     = "${TELEGRAM_BOT_TOKEN}"
allowed_users = ["12345678", "87654321"]   # only these user IDs can chat
```

**Default behavior** (identity-trust-none):
- No config → `allow_all_users` defaults to `false` → bot denies all users
- Set `allowed_users = ["12345678"]` → only listed IDs can chat
- Set `allow_all_users = true` → open to everyone (opt-in)

**Resolution order:** config value → `TELEGRAM_ALLOWED_USERS` env var (comma-separated) → empty (deny all).

> **Finding your Telegram user ID:** Send `/start` to [@userinfobot](https://t.me/userinfobot) or use the `getUpdates` API after messaging your bot.

### Set the Webhook

```bash
export BOT_TOKEN="your-bot-token"
export WEBHOOK_URL="https://your-public-url"
export SECRET="your-webhook-secret"

curl "https://api.telegram.org/bot${BOT_TOKEN}/setWebhook?url=${WEBHOOK_URL}/webhook/telegram&secret_token=${SECRET}"
```

---

## Standalone Gateway Mode (Legacy)

For deployments that need a separate gateway process (e.g., custom webhook routing, multi-gateway fan-out).

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

### File Attachments (Inbound)

The gateway downloads media from Telegram and stores it locally (`~/.openab/media/inbound/<uuid>`). Core reads directly from disk — no base64 encoding overhead.

| Type | Handling |
|------|----------|
| **Images** | Downloaded, resized (max 1200px), JPEG compressed, stored to filesystem. Agent sees the image. |
| **Documents** | Text-based files (`.txt`, `.csv`, `.rs`, `.py`, etc.) up to 20MB read as UTF-8 and passed to agent. Binary files silently skipped. |
| **Audio/Voice** | Downloaded and stored. If STT is enabled in Core, automatically transcribed and passed as text. |

**Not supported (inbound):** video, stickers, animations (silently skipped).
**Not supported (outbound):** bot cannot send images/files back to the user yet.

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

Agent replies are rendered with Telegram Markdown: **bold**, `code`, and code blocks work natively.

With **Rich Messages** enabled (default, requires Bot API 10.1+), headings (`##`) and tables render with full formatting via `sendRichMessage`. Code blocks remain on the legacy path for syntax highlighting and copy-button support. Content exceeding 4096 characters is automatically handled via rich messages (up to 32768 chars).

> **Note:** As of v0.9.0, OAB automatically disables table code-block wrapping for Telegram adapters (both unified and standalone gateway) when Rich Messages are enabled. Tables pass through as raw markdown and render natively. No `[markdown]` config is needed. To override this and force code-block wrapping, add:
>
> ```toml
> [markdown]
> tables = "code"
> ```
>
> Rich Messages requires gateway version **v0.6.0-rc.1** or above (`ghcr.io/openabdev/openab-gateway:v0.6.0-rc.1`+).

Set `TELEGRAM_RICH_MESSAGES=false` to disable rich messages and use legacy `sendMessage` for all replies.

## Environment Variables (Gateway)

| Variable | Required | Default | Description |
|---|---|---|---|
| `TELEGRAM_BOT_TOKEN` | Yes | — | Bot API token from @BotFather |
| `TELEGRAM_SECRET_TOKEN` | No | — | Webhook signature validation |
| `TELEGRAM_RICH_MESSAGES` | No | `true` | Use `sendRichMessage` for tables/headings/long content (Bot API 10.1+). Set `false` to opt out. |
| `TELEGRAM_STREAMING` | No | follows `TELEGRAM_RICH_MESSAGES` | Stream replies live via `sendRichMessageDraft`. Defaults to `true` when rich messages are enabled, `false` otherwise. Set `false` for send-once mode (single final message). |
| `TELEGRAM_ALLOWED_USERS` | No | — | Comma-separated Telegram user IDs allowed to interact with the bot. Empty = deny all. **Unified binary only** — standalone gateway uses `[gateway].allowed_users` instead. |
| `TELEGRAM_ALLOW_ALL_USERS` | No | `false` | Explicit flag: `true` = allow all users, `false` = check `allowed_users`. Defaults to `false` (deny-all, per identity-trust-none ADR). **Unified binary only.** |
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

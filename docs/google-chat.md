# Google Chat Setup

Connect a Google Chat app to OpenAB via the Custom Gateway.

```
Google Chat ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                          (OAB connects out)
```

## Prerequisites

- A running OAB instance (with kiro-cli or any ACP agent authenticated)
- The Custom Gateway deployed ([gateway/README.md](../gateway/README.md))
- A Google Cloud project with the Google Chat API enabled
- A Google Cloud Service Account (JSON key recommended; no special IAM roles needed)

## 1. Create a Google Chat App

1. Go to the [Google Cloud Console](https://console.cloud.google.com/) and create or select a project.
2. Enable the **Google Chat API** under **APIs & Services → Library**.
3. Go to **APIs & Services → Google Chat API → Configuration**:
   - **App name**: your bot name (e.g. "OpenAB")
   - **Avatar URL**: any public image URL
   - **Description**: anything
   - **Interactive features**: Enable
   - **Connection settings**: select **App URL** and enter your gateway's webhook URL:
     ```
     https://your-gateway-host/webhook/googlechat
     ```
   - **Visibility**: select the users or domains that can use the bot
4. Click **Save**.

## 2. Create a Service Account

Google Chat uses a service account to authenticate outbound API calls (bot replies).

1. Go to **IAM & Admin → Service Accounts** → **Create Service Account**.
2. Name it (e.g. `openab-google-chat`) and grant it no special roles.
3. After creation, click the service account → **Keys** → **Add Key** → **Create New Key** → JSON.
4. Save the downloaded JSON file securely.

## 3. Configure the Gateway

The gateway supports two authentication methods for sending replies:

### Option A: Service Account Key (recommended — auto-refresh)

Pass the service account JSON key directly. The gateway handles JWT signing and token refresh automatically.

```bash
# Via JSON string
docker run -d --name openab-gateway \
  -e GOOGLE_CHAT_ENABLED=true \
  -e GOOGLE_CHAT_SA_KEY_JSON='{"type":"service_account","client_email":"...","private_key":"..."}' \
  -e GATEWAY_WS_TOKEN="your-ws-auth-token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:latest

# Via file path
docker run -d --name openab-gateway \
  -e GOOGLE_CHAT_ENABLED=true \
  -e GOOGLE_CHAT_SA_KEY_FILE="/secrets/service-account.json" \
  -v /path/to/service-account.json:/secrets/service-account.json:ro \
  -e GATEWAY_WS_TOKEN="your-ws-auth-token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:latest
```

### Option B: Static Access Token (for quick testing)

Generate a token manually. It expires after 1 hour.

```bash
docker run -d --name openab-gateway \
  -e GOOGLE_CHAT_ENABLED=true \
  -e GOOGLE_CHAT_ACCESS_TOKEN="ya29.c..." \
  -e GATEWAY_WS_TOKEN="your-ws-auth-token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:latest
```

### Local development

```bash
export GOOGLE_CHAT_ENABLED=true
export GOOGLE_CHAT_SA_KEY_FILE="/path/to/service-account.json"
cargo run --release
```

## 4. Expose the Gateway (for local dev)

Google Chat requires a public HTTPS endpoint for webhooks.

### Cloudflare Tunnel (quickest)

```bash
cloudflared tunnel --url http://localhost:8080
# Copy the https://xxx.trycloudflare.com URL
```

Then update the webhook URL in the Google Chat API Configuration page:
```
https://xxx.trycloudflare.com/webhook/googlechat
```

### Reverse proxy (production)

Use nginx, Caddy, or a cloud load balancer with TLS termination pointing to the gateway's `:8080`.

## 5. Configure OAB

```toml
[gateway]
url = "ws://openab-gateway:8080/ws"
platform = "googlechat"
allow_all_channels = true
allow_all_users = true

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

## Features

### Supported

- **DM chat** — send a direct message to the bot, get an AI agent response
- **Space chat** — add the bot to a Google Chat Space, @mention it to start a conversation
- **Thread replies** — in Spaces, bot replies are posted in the same thread as the user's message (note: @mention is required for every message in a Space, even within a thread — this is a Google Chat platform limitation)
- **`argument_text` extraction** — strips the @mention prefix to get the clean user message
- **Bot message filtering** — bot messages (`user_type: "BOT"`) are filtered at the gateway level
- **Message splitting** — long replies (>4096 chars) are automatically split at newline/space boundaries
- **Token auto-refresh** — service account JWT tokens are refreshed automatically before expiry
- **Markdown formatting** — replies are converted via `markdown_to_gchat` to Google Chat's native formatting:
  - Bold: `**text**` / `__text__` → `*text*`
  - Italic: `*text*` → `_text_` (single-underscore `_text_` passes through)
  - Strikethrough: `~~text~~` → `~text~`
  - Headings: `# / ## / ###` → `*text*` (rendered as bold)
  - Links: `[text](url)` → `<url|text>`
  - Inline code, fenced code blocks: pass through unchanged
  - Tables and other unsupported syntax pass through as-is
- **Streaming (edit_message)** — when OAB streaming is enabled, the bot edits its initial reply in-place as tokens arrive (typewriter effect)

### Not Supported

- **Reactions** — Google Chat API does not support message reactions on behalf of bots
- **File/image attachments** — not yet implemented

## Environment Variables (Gateway)

| Variable | Required | Default | Description |
|---|---|---|---|
| `GOOGLE_CHAT_ENABLED` | Yes | `false` | Set to `true` or `1` to enable the adapter |
| `GOOGLE_CHAT_AUDIENCE` | Recommended | — | JWT audience for webhook verification — set to your full webhook URL (e.g. `https://your-domain.com/webhook/googlechat`) |
| `GOOGLE_CHAT_SA_KEY_JSON` | No | — | Service account key JSON string (enables auto-refresh) |
| `GOOGLE_CHAT_SA_KEY_FILE` | No | — | Path to service account key JSON file (alternative to `SA_KEY_JSON`) |
| `GOOGLE_CHAT_ACCESS_TOKEN` | No | — | Static OAuth2 access token (fallback, expires in 1 hour) |
| `GOOGLE_CHAT_WEBHOOK_PATH` | No | `/webhook/googlechat` | Webhook endpoint path |

## Security: Webhook Verification

Google Chat signs every webhook request with a JWT Bearer token. The gateway verifies this token to ensure requests come from Google Chat specifically (not just any Google service).

**Setup:**

In the Google Chat API **Configuration** page, leave **Authentication Audience** at its default — **HTTP Endpoint URL**. Then set `GOOGLE_CHAT_AUDIENCE` to your full webhook URL:

```bash
export GOOGLE_CHAT_AUDIENCE="https://your-domain.com/webhook/googlechat"
```

The gateway will:
- Reject requests without a valid `Authorization: Bearer <jwt>` header
- Verify the JWT signature against Google's public keys (JWKS, cached for 1 hour)
- Validate `iss == https://accounts.google.com` and `aud` matches the configured webhook URL
- Validate `email` ends with `@gcp-sa-gsuiteaddons.iam.gserviceaccount.com` (proves the token came from Google Chat, not another Google service)

If `GOOGLE_CHAT_AUDIENCE` is not set, the gateway logs a warning and accepts all requests (insecure — for local development only).

> **Note:** Only the "HTTP Endpoint URL" Authentication Audience mode is supported. The "Project Number" mode uses a different JWT flow that this adapter does not implement.

## Troubleshooting

| Problem | Fix |
|---|---|
| Bot doesn't respond | Check `GOOGLE_CHAT_ENABLED=true` is set. Check gateway logs for parse errors. |
| "not responding" in Google Chat | Ensure the gateway returns a `200` with `{}` body. Check gateway is reachable via the webhook URL. |
| Replies not sent | Use `GOOGLE_CHAT_SA_KEY_JSON` or `GOOGLE_CHAT_SA_KEY_FILE` for auto-refresh. If using static token, check it hasn't expired (1-hour TTL). |
| Replies not in thread | Verify the thread name is passed correctly. The gateway appends `?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD` automatically. |
| Bot responds to its own messages | Bot messages have `user_type: "BOT"` and are filtered out automatically. |
| Webhook returns 400 | Check the Google Chat API configuration uses **App URL** (not Dialogflow or Cloud Pub/Sub). The webhook expects the v2 envelope format with a `chat` wrapper. |

## References

- [Google Chat API Documentation](https://developers.google.com/workspace/chat/api/reference/rest)
- [Google Chat App Setup](https://developers.google.com/workspace/chat/overview)
- [Service Account Authentication](https://developers.google.com/workspace/chat/authenticate-authorize-chat-app)

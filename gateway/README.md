# OpenAB Custom Gateway

A standalone service that bridges webhook-based platforms and custom event sources to OAB via WebSocket. OAB connects outbound to the gateway — no inbound ports or TLS required on OAB.

```
                 External (HTTPS)                    Internal (cluster)
                 ────────────────                    ──────────────────

Telegram  ──POST──▶┌─────────────────────┐
LINE      ──POST──▶│                     │
GitHub    ──POST──▶│   Custom Gateway    │◀──WebSocket── OAB Pod
CI/CD     ──POST──▶│     :8080           │   (OAB connects out)
curl/cron ──POST──▶│                     │
                   └─────────────────────┘

Discord  ◀──WebSocket── OAB Pod  (unchanged, direct)
Slack    ◀──WebSocket── OAB Pod  (unchanged, direct)
```

The gateway normalizes all inbound events to a unified schema (`openab.gateway.event.v1`), forwards them to OAB over WebSocket, and routes OAB replies back to the originating platform API.

For architecture details, see [ADR: Custom Gateway](../docs/adr/custom-gateway.md).

> **Design note:** The gateway is intentionally NOT included in the OAB container image. It is a separate service with its own build, deployment, and scaling lifecycle. This follows the ADR principle that OAB remains outbound-only and platform-agnostic — all inbound webhook handling and platform credentials live in the gateway.

---

## Quick Start

```bash
cargo build --release
export TELEGRAM_BOT_TOKEN="your-bot-token"
./target/release/openab-gateway
```

### OAB Config

```toml
[gateway]
url = "ws://gateway:8080/ws"
```

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `TELEGRAM_BOT_TOKEN` | (required) | Telegram Bot API token |
| `GATEWAY_LISTEN` | `0.0.0.0:8080` | Listen address |
| `TELEGRAM_WEBHOOK_PATH` | `/webhook/telegram` | Webhook endpoint path |

### Endpoints

| Path | Description |
|---|---|
| `POST /webhook/telegram` | Telegram webhook receiver |
| `GET /ws` | WebSocket server (OAB connects here) |
| `GET /health` | Health check |

---

## Platform Setup

### Telegram

1. Create a bot via [@BotFather](https://t.me/BotFather) and get the token.

2. Start the gateway:
   ```bash
   export TELEGRAM_BOT_TOKEN="your-token"
   ./target/release/openab-gateway
   ```

3. Expose the gateway over HTTPS (Telegram requires it). Easiest option — Cloudflare Tunnel:
   ```bash
   cloudflared tunnel --url http://localhost:8080
   ```

4. Set the webhook:
   ```bash
   curl "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/setWebhook?url=https://your-host/webhook/telegram"
   ```

5. For supergroup forum topics (thread isolation like Discord), give the bot **Manage Topics** permission in the group settings.

### LINE (TBD)

LINE adapter is planned. It will follow the same pattern:
- Webhook at `/webhook/line`
- Signature validation via `X-Line-Signature` header
- Reply via LINE Push Message API

See [ADR: LINE Adapter](../docs/adr/line-adapter.md) for the design.

### Other Platforms (TBD)

GitHub webhooks, CI/CD events, monitoring alerts — any HTTP event source can be added as a gateway adapter. See the ADR for the adapter interface.

---

## Custom Event Source

Any HTTP client can drive an OAB agent session by posting to the webhook endpoint. This turns OAB into an event-driven agent platform — no chat app required.

### Example: trigger an agent from a cron job

```bash
curl -X POST http://gateway:8080/webhook/telegram \
  -H "Content-Type: application/json" \
  -d '{
    "message": {
      "message_id": 1,
      "chat": {"id": 12345, "type": "private"},
      "from": {"id": 99, "first_name": "CronJob", "username": "scheduler", "is_bot": false},
      "text": "run daily security scan on staging"
    }
  }'
```

### Example: generic event (future `/webhook/custom` endpoint)

Once a generic webhook adapter is added, any JSON payload can trigger an agent:

```bash
curl -X POST http://gateway:8080/webhook/custom \
  -H "Content-Type: application/json" \
  -d '{
    "channel": "ops-alerts",
    "sender": "cloudwatch",
    "text": "CPU > 90% on prod-api-3 for 5 minutes, investigate and suggest fix"
  }'
```

The agent response is delivered back through the gateway to whatever reply mechanism the adapter defines — Telegram message, GitHub comment, Slack DM, PagerDuty note, or simply logged.

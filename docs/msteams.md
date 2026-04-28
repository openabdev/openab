# Microsoft Teams Setup

Connect a Microsoft Teams bot to OpenAB via the Custom Gateway.

```
Teams (Bot Framework) ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                                  (OAB connects out)
                       ◀──REST──── (Bot Framework reply)
```

## Prerequisites

- A running OAB instance (with kiro-cli or any ACP agent authenticated)
- The Custom Gateway deployed ([gateway/README.md](../gateway/README.md))
- A Microsoft 365 / Azure AD account with permission to register apps and create Azure Bot resources
- A public HTTPS URL for the gateway (Cloudflare Tunnel, ngrok, k8s ingress, etc.) — Bot Framework will not call HTTP endpoints

## 1. Register an Azure AD Application

1. Go to [Azure Portal → App registrations](https://portal.azure.com/#blade/Microsoft_AAD_RegisteredApps/ApplicationsListBlade) → **New registration**
2. Name: `openab-teams-bot` (or anything you like)
3. **Supported account types**:
   - **Single tenant** — only your organization can use the bot (most common for internal use)
   - **Multitenant** — anyone with a Microsoft 365 account can install
4. Leave **Redirect URI** empty → Register

After creation, copy from the **Overview** page:
- **Application (client) ID** → `TEAMS_APP_ID`
- **Directory (tenant) ID** → needed for `TEAMS_OAUTH_ENDPOINT` if Single tenant

Then go to **Certificates & secrets** → **New client secret** → copy the **Value** (not the Secret ID) → `TEAMS_APP_SECRET`.

> Client secrets are only shown once. Store it before leaving the page.

## 2. Create an Azure Bot Resource

1. Azure Portal → **Create a resource** → search **Azure Bot** → Create
2. **Bot handle**: pick a unique name (e.g. `openab`)
3. **Subscription / Resource group**: pick yours
4. **Pricing tier**: F0 (free) is fine for testing
5. **Microsoft App ID**:
   - **Type of App**: must match what you picked in step 1 (`Single Tenant` or `Multi Tenant`)
   - **Creation type**: **Use existing app registration**
   - **App ID**: paste the `TEAMS_APP_ID` from step 1
   - **App tenant ID** (Single tenant only): paste your tenant ID
6. Review + create

After deployment, open the bot:
- **Configuration** → **Messaging endpoint**: `https://gw.yourdomain.com/webhook/teams` (the gateway's public URL)
- **Channels** → click **Microsoft Teams** → accept terms → save

## 3. Build a Teams App Manifest

Bot Framework only delivers messages once a Teams app installs your bot. You have two paths:

### Option A — Teams Developer Portal (UI)

In [Teams Developer Portal](https://dev.teams.microsoft.com) → **Apps** → **New app**:

1. **Basic information** → fill name, description, developer info
2. **App features** → **Bot** → **Create new bot** → select **Use existing bot ID** → paste `TEAMS_APP_ID`
3. Pick the scopes the bot needs:
   - **Personal** — 1:1 chat
   - **Team** — channel chat (must be @mentioned)
   - **Group chat** — multi-person DMs
4. **Publish** → **Publish to your org** (single tenant) or sideload via **Apps for your org**

### Option B — Hand-rolled `manifest.json`

If you'd rather build the manifest yourself and sideload as a `.zip`, drop this in `manifest.json` next to two icons (`outline.png` — transparent 32×32 white, `color.png` — 192×192 colored), zip them, and in Teams: **Apps → Manage your apps → Upload a custom app**.

```json
{
  "$schema": "https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json",
  "manifestVersion": "1.25",
  "version": "1.0.0",
  "id": "<generate-a-fresh-uuid-v4>",
  "developer": {
    "name": "<Your Org>",
    "websiteUrl": "https://example.com",
    "privacyUrl": "https://example.com/privacy",
    "termsOfUseUrl": "https://example.com/terms"
  },
  "name": {
    "short": "<bot-short-name>",
    "full": "<Bot Full Name>"
  },
  "description": {
    "short": "<one-line bot description>",
    "full": "<longer description shown on the app details page>"
  },
  "icons": {
    "outline": "outline.png",
    "color": "color.png"
  },
  "accentColor": "#ffffff",
  "bots": [
    {
      "botId": "<TEAMS_BOT_ID>",
      "scopes": ["personal", "team", "groupChat"],
      "isNotificationOnly": false,
      "supportsFiles": false
    }
  ],
  "validDomains": []
}
```

Notes:
- `id` is the **Teams app id** — generate a fresh UUID v4 (`uuidgen` on macOS). It is **not** the same as `botId`.
- `botId` is the **Microsoft App (Bot) id** from step 1 (the value you put in `TEAMS_APP_ID`).
- The three `developer.*` URLs are required by the schema. They can point at your GitHub repo / privacy page / license — they just have to resolve.
- Leave `supportsFiles: false` until OAB's Teams adapter actually handles file attachments.
- Drop fields like `defaultGroupCapability`, `supportsChannelFeatures`, `composeExtensions`, `staticTabs` unless you're adding tabs / messaging extensions / meeting features. They don't apply to a bot-only app.

> If your tenant requires admin approval, an admin must approve the published app in Teams Admin Center → Manage apps.

## 4. Configure the Gateway

Add the Teams env vars to your gateway deployment:

```bash
# Docker
docker run -d --name openab-gateway \
  -e TEAMS_APP_ID="<application-id>" \
  -e TEAMS_APP_SECRET="<client-secret-value>" \
  -e TEAMS_OAUTH_ENDPOINT="https://login.microsoftonline.com/<tenant-id>/oauth2/v2.0/token" \
  -p 8080:8080 \
  ghcr.io/openabdev/openab-gateway:latest

# Kubernetes
kubectl set env deployment/openab-gateway \
  TEAMS_APP_ID="<application-id>" \
  TEAMS_APP_SECRET="<client-secret-value>" \
  TEAMS_OAUTH_ENDPOINT="https://login.microsoftonline.com/<tenant-id>/oauth2/v2.0/token"
```

> **Critical for Single tenant bots**: `TEAMS_OAUTH_ENDPOINT` **must** point at your tenant. The default
> `https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token` only works for Multi tenant bots.
> Setting the wrong endpoint causes `Bot Framework API error 401 Unauthorized: Authorization has been
> denied for this request` when the gateway tries to reply.

## 5. Configure OAB

`config.toml` on the OAB side just points at the gateway. Teams routing is handled by the gateway, not OAB:

```toml
[gateway]
url = "ws://openab-gateway:8080/ws"
platform = "teams"

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

## 6. Install the Bot in Teams

In Teams (web or desktop):

1. **Apps** → **Manage your apps** → **Built for your org** → find your app → **Add**
2. For personal chat: open the app, start chatting
3. For a channel: click the app → **Add to a team** → choose the team → **Set up a tab** or just use `@<bot-name>` in conversation

Once the bot is installed, the first message triggers a webhook to `/webhook/teams`. The gateway caches the conversation's `service_url` so it can reply.

## Self-hosting: Docker Compose stack

Convenience stack that runs the gateway and OAB together. A Cloudflare Tunnel service is included as **one example** of how to expose the gateway publicly — see [Public HTTPS exposure](#public-https-exposure-cloudflare-tunnel-is-optional) below if you want to use ngrok / k8s ingress / Tailscale Funnel / your own reverse proxy instead.

Drop these three files into a project directory and `docker compose up -d`.

### `.env`

```dotenv
# From Azure AD app registration (step 1)
TEAMS_APP_ID="<application-id>"
TEAMS_APP_SECRET="<client-secret-value>"

# Single tenant: must point at your tenant
TEAMS_OAUTH_ENDPOINT="https://login.microsoftonline.com/<tenant-id>/oauth2/v2.0/token"

# Optional — defaults shown
TEAMS_WEBHOOK_PATH="/webhook/teams"

# Only needed if you use the Cloudflare Tunnel service below.
# Skip this line if you expose the gateway via a different reverse proxy.
TUNNEL_TOKEN="<your-cloudflare-tunnel-token>"

RUST_LOG=info
```

`.env` should be `.gitignore`d — it holds your bot secret.

### `docker-compose.yaml`

```yaml
services:
  gateway:
    image: ghcr.io/openabdev/openab-gateway:latest
    container_name: gateway
    env_file:
      - .env
    ports:
      - 8080:8080

  openab:
    image: ghcr.io/openabdev/openab:latest
    container_name: openab
    volumes:
      - ./config.toml:/etc/openab/config.toml
      - ./data:/home/agent
    env_file:
      - .env
    depends_on:
      - gateway

  # Optional — only include this service if you want to use Cloudflare Tunnel
  # to expose the gateway publicly. Drop this whole block if you reverse-proxy
  # gateway:8080 some other way (see "Public HTTPS exposure" below).
  tunnels:
    image: cloudflare/cloudflared:latest
    command: tunnel --no-autoupdate run --token ${TUNNEL_TOKEN}
    env_file:
      - .env
    depends_on:
      - gateway
      - openab
```

### `config.toml` (mounted into the `openab` container)

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

[pool]
max_sessions = 10
session_ttl_hours = 24

[reactions]
enabled = true

[gateway]
url = "ws://gateway:8080/ws"
platform = "teams"
```

### Run it

```fish
docker compose up -d
docker compose logs -f gateway openab
```

### Public HTTPS exposure (Cloudflare Tunnel is optional)

Bot Framework needs to reach the gateway over HTTPS. **Any reverse proxy works** — Cloudflare Tunnel is just convenient for self-hosting because it terminates TLS for you and doesn't need a static IP. Pick whichever fits:

#### Option A — Cloudflare Tunnel (the example shown above)

In the [Cloudflare Zero Trust dashboard](https://one.dash.cloudflare.com/), open your tunnel and add a public hostname:

| Field | Value |
|---|---|
| Subdomain / Hostname | `openab-bot` (or anything) |
| Path | `/webhook/teams` |
| Service type | `HTTP` |
| URL | `gateway:8080` |

#### Option B — ngrok / Tailscale Funnel / your own reverse proxy

```fish
# ngrok example
ngrok http 8080
# → https://<random>.ngrok-free.app/webhook/teams
```

Drop the `tunnels` service and the `TUNNEL_TOKEN` line in `.env`; just expose `gateway:8080` to the internet however you prefer (k8s ingress, Caddy, nginx + Let's Encrypt, Tailscale Funnel, etc.).

#### Then point Bot Framework at it

Azure Portal → your bot → **Configuration** → **Messaging endpoint**:
`https://<your-public-host>/webhook/teams`

## Features

### Supported

- **1:1 personal chat** — direct message the bot, get an agent response
- **Channel chat** — bot responds when @mentioned (so it doesn't flood every channel message)
- **Group chat** — same @mention gating
- **JWT validation** — every webhook is verified against Microsoft's public JWKS
- **Markdown rendering** — replies are sent with `textFormat: "markdown"`
- **Tenant allowlist** — set `TEAMS_ALLOWED_TENANTS=<tenant-id-1>,<tenant-id-2>` to restrict which tenants can talk to the bot

### Not Supported (Teams API limitations)

- **Reactions** — Teams Bot Framework reactions API exists but is not yet wired up. The status reactions OAB sends (👀 / 🤔 / ⚡ / 🆗) are silently dropped for Teams replies.
- **Thread replies** — Teams "channel threads" map to a single conversation in Bot Framework. All messages in a personal chat or channel share one agent session.
- **Streaming edits** — replies are sent as one final message, not progressively edited.

## Environment Variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `TEAMS_APP_ID` | Yes | — | Azure AD application (client) ID |
| `TEAMS_APP_SECRET` | Yes | — | Azure AD client secret value |
| `TEAMS_OAUTH_ENDPOINT` | Single tenant: Yes | `https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token` | OAuth token endpoint. **Single tenant** bots must override this to `https://login.microsoftonline.com/<tenant-id>/oauth2/v2.0/token` |
| `TEAMS_OPENID_METADATA` | No | `https://login.botframework.com/v1/.well-known/openidconfiguration` | OpenID metadata for inbound JWT signing keys. Public Bot Framework default works for both single and multi tenant. |
| `TEAMS_ALLOWED_TENANTS` | No | (allow all) | Comma-separated tenant IDs. If set, webhooks from other tenants get HTTP 403. |
| `TEAMS_WEBHOOK_PATH` | No | `/webhook/teams` | URL path the gateway listens on |

## Troubleshooting

**Bot Framework API error 401 Unauthorized: Authorization has been denied for this request**
- Almost always means OAuth endpoint vs. app type mismatch.
- Single tenant bot → set `TEAMS_OAUTH_ENDPOINT=https://login.microsoftonline.com/<tenant-id>/oauth2/v2.0/token`
- Multi tenant bot → leave default, but verify `TEAMS_APP_ID` and `TEAMS_APP_SECRET` are correct (secret may have expired in Azure AD).

**`teams: no service_url for conversation` in gateway logs**
- The bot is trying to reply to a conversation it never received an inbound message for. Possibilities:
  - Gateway was restarted and the in-memory `service_url` cache was cleared. Have the user send another message.
  - The webhook never arrived (check Bot Framework webhook URL points at the right gateway).

**`teams JWT validation failed` in gateway logs**
- The inbound token's signing key is not in the cached JWKS. The gateway auto-refreshes JWKS on miss, so this usually resolves on retry. If it persists, check `TEAMS_OPENID_METADATA` is reachable from the gateway pod.

**Webhook returns 200 but no agent response**
- Run `docker compose logs gateway openab` and look for the trace:
  1. `teams → gateway` (gateway received webhook)
  2. `processing message channel_platform=teams` (OAB picked up the event)
  3. `sending reply to gateway platform=teams` (OAB sent the reply over WS)
  4. `OAB → gateway reply platform=teams` (gateway received the reply)
  5. `gateway → teams conversation=...` (gateway is calling Bot Framework REST API)
  6. `teams activity sent` (success) or `teams send error` (failure)
- Whichever step is missing tells you where the break is.

**Bot doesn't appear when @mentioning in a channel**
- The Teams app must be installed in the team (Apps → Built for your org → Add to a team).
- If your tenant blocks third-party apps, an admin must approve in Teams Admin Center → Manage apps.

## References

- [Bot Framework REST API](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference)
- [Azure Bot Service authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication)
- [Teams Developer Portal](https://dev.teams.microsoft.com)
- [Teams app manifest schema](https://learn.microsoft.com/en-us/microsoftteams/platform/resources/schema/manifest-schema)
- [ADR: Custom Gateway](../docs/adr/custom-gateway.md)

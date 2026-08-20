# Microsoft Teams Setup (Self-Hosted)

> **Unified Mode (v0.9.0+):** The OAB binary now embeds the Teams adapter directly. Set `TEAMS_APP_ID` as an env var — no separate gateway container or `[gateway]` config needed. See [Telegram docs](telegram.md#unified-mode-recommended) for the pattern.

### Unified Config (Kiro + Teams)

**Minimal:**

```toml
[agent]
env = { KIRO_API_KEY = "${KIRO_API_KEY}" }
```

**Recommended:**

```toml
[agent]
env = { KIRO_API_KEY = "${KIRO_API_KEY}" }

[pool]
max_sessions = 3
session_ttl_hours = 1

[reactions]
tool_display = "compact"

[markdown]
tables = "off"
```

Set `TEAMS_APP_ID` and `TEAMS_APP_SECRET` on the container. No `[gateway]` needed.

### `[teams]` Section (credentials + trust)

Since #1380 the `[teams]` section carries the full adapter configuration — config-first with `TEAMS_*` env fallback:

```toml
[teams]
app_id     = "${TEAMS_APP_ID}"
app_secret = "${TEAMS_APP_SECRET}"
allowed_tenants = ["<tenant-guid>"]
allowed_users   = ["29:1abc..."]
```

### User Trust (`[teams]` section)

Identity trust defaults to **deny-all** (identity-trust-none ADR): unknown senders are rejected until explicitly admitted. Configure trust with a first-class `[teams]` section:

```toml
[teams]
allowed_users = ["29:1abc..."]  # Bot Framework activity.from.id values
# allow_all_users = true   # explicit opt-in only — any user can drive the agent
```

Each field falls back to its `TEAMS_ALLOW_ALL_USERS` / `TEAMS_ALLOWED_USERS` env var when unset. To find a user's `29:…` ID, check the OAB logs — the sender ID is logged for each incoming message.

> **Trust resolution:** the `[teams]` section's trust settings apply in **both** deployment modes. Broker-side enforcement goes through the shared per-platform trust registry with precedence `GATEWAY_*` env < `[gateway]` section < `[teams]` section — in the standalone-gateway mode, the broker's WebSocket path consults the same registry, so a `[teams]` section overrides `[gateway].allow_all_users` / `allowed_users` for this platform.

> ⚠️ **Deprecated:** driving Teams trust through the uniform `GATEWAY_ALLOW_ALL_USERS` / `GATEWAY_ALLOWED_USERS` env vars still works but logs a startup warning; it will become a startup error in a later phase. Migrate to `[teams]` (or `TEAMS_*` env vars).

---

## Standalone Gateway Mode (Legacy)

Connect a Microsoft Teams bot to OpenAB via the Custom Gateway using a self-hosted Docker Compose stack.

```
Teams (Bot Framework) ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                                  (OAB connects out)
                       ◀──REST──── (Bot Framework reply)
```

## Prerequisites

- Docker and Docker Compose installed
- A Microsoft 365 / Azure AD account with permission to register apps and create Azure Bot resources
- A public HTTPS URL for the gateway (Cloudflare Tunnel, ngrok, Tailscale Funnel, etc.) — Bot Framework requires HTTPS endpoints

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
6. Review + Create

After deployment, open the bot:

- **Configuration** → **Messaging endpoint**: `https://<YOUR_PUBLIC_HOST>/webhook/teams`
- **Channels** → click **Microsoft Teams** → accept terms → save

## 3. Build a Teams App Manifest

Bot Framework only delivers messages once a Teams app installs your bot.

### Option A — Teams Developer Portal (UI)

In [Teams Developer Portal](https://dev.teams.microsoft.com) → **Apps** → **New app**:

1. **Basic information** → fill name, description, developer info
2. **App features** → **Bot** → **Create new bot** → select **Use existing bot ID** → paste `TEAMS_APP_ID`
3. Pick the scopes the bot needs:
   - **Personal** — 1:1 chat
   - **Team** — channel chat (must be @mentioned)
   - **Group chat** — multi-person DMs
4. **Publish** → **Publish to your org** (single tenant) or sideload via **Apps for your org**

### Option B — Hand-rolled manifest.json

Create `manifest.json` next to two icons (`outline.png` — transparent 32×32 white, `color.png` — 192×192 colored), zip them, and in Teams: **Apps → Manage your apps → Upload a custom app**.

```json
{
  "$schema": "https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json",
  "manifestVersion": "1.25",
  "version": "1.0.0",
  "id": "<GENERATE_A_UUID_V4>",
  "developer": {
    "name": "<YOUR_ORG>",
    "websiteUrl": "https://example.com",
    "privacyUrl": "https://example.com/privacy",
    "termsOfUseUrl": "https://example.com/terms"
  },
  "name": {
    "short": "<YOUR_BOT_SHORT_NAME>",
    "full": "<YOUR_BOT_FULL_NAME>"
  },
  "description": {
    "short": "<YOUR_BOT_SHORT_DESCRIPTION>",
    "full": "<YOUR_BOT_FULL_DESCRIPTION>"
  },
  "icons": {
    "outline": "outline.png",
    "color": "color.png"
  },
  "accentColor": "#ffffff",
  "bots": [
    {
      "botId": "<YOUR_TEAMS_APP_ID>",
      "scopes": ["personal", "team", "groupChat"],
      "isNotificationOnly": false,
      "supportsFiles": false
    }
  ],
  "validDomains": []
}
```

Notes:

- `id` is the **Teams app id** — generate a fresh UUID v4 (`uuidgen`). It is **not** the same as `botId`.
- `botId` is the **Microsoft App (Bot) id** from step 1 (the value you put in `TEAMS_APP_ID`).
- The three `developer.*` URLs are required by the schema. They can point at your GitHub repo / privacy page / license — they just have to resolve.

> If your tenant requires admin approval, an admin must approve the published app in Teams Admin Center → Manage apps.

## 4. Self-Hosted Deployment (Docker Compose)

Drop these three files into a project directory and run `docker compose up -d`.

### `.env`

```env
# From Azure AD app registration (step 1)
TEAMS_APP_ID="<YOUR_APPLICATION_ID>"
TEAMS_APP_SECRET="<YOUR_CLIENT_SECRET>"

# Single tenant: must point at your tenant
# Multi tenant: leave this line out (uses default)
TEAMS_OAUTH_ENDPOINT="https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token"

# Only needed if you use the Cloudflare Tunnel service below.
# Skip this line if you expose the gateway via a different reverse proxy.
TUNNEL_TOKEN="<YOUR_CLOUDFLARE_TUNNEL_TOKEN>"

RUST_LOG=info
```

> `.env` should be `.gitignore`d — it holds your bot secret.

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

  # Optional — only include this service if you want to use Cloudflare Tunnel.
  # Drop this block if you reverse-proxy gateway:8080 some other way.
  tunnels:
    image: cloudflare/cloudflared:latest
    command: tunnel --no-autoupdate run --token ${TUNNEL_TOKEN}
    env_file:
      - .env
    depends_on:
      - gateway
      - openab
```

### `config.toml`

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

### Start the stack

```bash
docker compose up -d
docker compose logs -f gateway openab
```

## 5. Public HTTPS Exposure

Bot Framework needs to reach the gateway over HTTPS. Any reverse proxy works — pick whichever fits your setup.

### Option A — Cloudflare Tunnel

In the [Cloudflare Zero Trust dashboard](https://one.dash.cloudflare.com/), open your tunnel and add a public hostname:

| Field | Value |
|---|---|
| Subdomain / Hostname | `openab-bot` (or anything) |
| Path | `/webhook/teams` |
| Service type | `HTTP` |
| URL | `gateway:8080` |

### Option B — ngrok / Tailscale Funnel / other reverse proxy

```bash
# ngrok example
ngrok http 8080
# → https://<random>.ngrok-free.app/webhook/teams
```

Drop the `tunnels` service and the `TUNNEL_TOKEN` line in `.env`; just expose `gateway:8080` to the internet however you prefer (k8s ingress, Caddy, nginx + Let's Encrypt, Tailscale Funnel, etc.).

### Point Bot Framework at your endpoint

Azure Portal → your bot → **Configuration** → **Messaging endpoint**: `https://<YOUR_PUBLIC_HOST>/webhook/teams`

## 6. Install the Bot in Teams

1. **Apps** → **Manage your apps** → **Built for your org** → find your app → **Add**
2. For personal chat: open the app, start chatting
3. For a channel: click the app → **Add to a team** → choose the team → use `@<bot-name>` in conversation

## Supported Features

- **1:1 personal chat** — direct message the bot, get an agent response
- **Channel chat** — bot responds when @mentioned
- **Group chat** — same @mention gating
- **JWT validation** — every webhook is verified against Microsoft's public JWKS
- **Markdown rendering** — replies are sent with `textFormat: "markdown"`
- **Tenant allowlist** — set `TEAMS_ALLOWED_TENANTS=<tenant-id-1>,<tenant-id-2>` to restrict which tenants can talk to the bot

## Current Limitations

- **Reactions** — status reactions (👀 / 🤔 / ⚡ / 🆗) are silently dropped for Teams replies
- **Thread replies** — all messages in a personal chat or channel share one agent session
- **Streaming edits** — replies are sent as one final message, not progressively edited

## Environment Variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `TEAMS_APP_ID` | Yes | — | Azure AD application (client) ID |
| `TEAMS_APP_SECRET` | Yes | — | Azure AD client secret value |
| `TEAMS_OAUTH_ENDPOINT` | Single tenant: Yes | `https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token` | HTTPS endpoint on `login.microsoftonline.com`; override the tenant path for single-tenant bots |
| `TEAMS_OPENID_METADATA` | No | `https://login.botframework.com/v1/.well-known/openidconfiguration` | HTTPS metadata endpoint on `login.botframework.com` |
| `TEAMS_ALLOWED_TENANTS` | No | (allow all) | Comma-separated tenant IDs |
| `TEAMS_WEBHOOK_PATH` | No | `/webhook/teams` | URL path the gateway listens on |

> ⚠️ **M0 supports Microsoft commercial public cloud only.** Sovereign-cloud endpoints and custom OAuth/OpenID proxy hosts are rejected. Bot Connector replies accept only validated HTTPS service URLs on `smba.trafficmanager.net`; redirects cannot cross origin.

## Troubleshooting

**401 Unauthorized when bot tries to reply**

- Almost always means OAuth endpoint vs. app type mismatch.
- Single tenant bot → set `TEAMS_OAUTH_ENDPOINT=https://login.microsoftonline.com/<YOUR_TENANT_ID>/oauth2/v2.0/token`
- Multi tenant bot → leave default, but verify `TEAMS_APP_ID` and `TEAMS_APP_SECRET` are correct.

**`teams: no service_url for conversation` in gateway logs**

- Gateway was restarted and the in-memory cache was cleared. Have the user send another message.
- Or the webhook never arrived — check Bot Framework webhook URL points at the right gateway.

**`teams JWT validation failed` in gateway logs**

- The gateway auto-refreshes JWKS on miss, so this usually resolves on retry.
- If it persists, check `TEAMS_OPENID_METADATA` is reachable from the gateway container.

**Webhook returns 200 but no agent response**

Check `docker compose logs gateway openab` and look for the trace:

1. `teams → gateway` (gateway received webhook)
2. `processing message channel_platform=teams` (OAB picked up the event)
3. `sending reply to gateway platform=teams` (OAB sent the reply over WS)
4. `gateway → teams` (gateway calling Bot Framework REST API)
5. `teams activity sent` (success) or `teams reply rejected` (failure)

Whichever step is missing tells you where the break is.

**Bot doesn't appear when @mentioning in a channel**

- The Teams app must be installed in the team (Apps → Built for your org → Add to a team).
- If your tenant blocks third-party apps, an admin must approve in Teams Admin Center → Manage apps.

## References

- [Bot Framework REST API](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference)
- [Azure Bot Service authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication)
- [Teams Developer Portal](https://dev.teams.microsoft.com)
- [Teams app manifest schema](https://learn.microsoft.com/en-us/microsoftteams/platform/resources/schema/manifest-schema)

# Google Chat Setup


> **Unified Mode (v0.9.0+):** The OAB binary now embeds the google-chat adapter directly. Set `GOOGLE_CHAT_ENABLED=true` as an env var — no separate gateway container or `[gateway]` config needed. See [Telegram docs](telegram.md#unified-mode-recommended) for the pattern.

### Unified Config (Kiro + google-chat)

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

Set `GOOGLE_CHAT_ENABLED=true` (and related platform env vars) on the container. No `[gateway]` needed.


Connect a Google Chat app to OpenAB via the Custom Gateway.

```
Google Chat ──POST──▶ Gateway (:8080) ◀──WebSocket── OAB Pod
                                          (OAB connects out)
```

## Prerequisites

- **A Google Workspace (Business or Enterprise) account** — required by Google to configure the Chat API. Regular `@gmail.com` consumer accounts cannot create Google Chat apps. Workspace Individual or Business Starter is the cheapest qualifying tier. See [Configure the Google Chat API](https://developers.google.com/workspace/chat/configure-chat-api).
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

The gateway supports three authentication methods for sending replies:

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

### Option C: Keyless ADC (recommended on GCP — no key file)

When the gateway runs on GCP (GKE / GCE / Cloud Run), its attached **runtime service account** can impersonate a separate **Google Chat service account** and mint a `chat.bot` token without any key file. The gateway reads the runtime identity and a base token from the GCE metadata server, then calls IAM Credentials `generateAccessToken` for the configured Chat-app target identity.

The two identities **must be different**. Google prohibits using a service account's short-lived access token to generate another access token for that same service account (`FAILED_PRECONDITION`); see [Service account credentials — Self-impersonation](https://cloud.google.com/iam/docs/service-account-creds#self-impersonation).

Prerequisites:

- Attach a runtime SA to the workload (for example `openab-runtime@PROJECT.iam.gserviceaccount.com`).
- Use a distinct SA as the Google Chat app identity (for example `openab-chat@PROJECT.iam.gserviceaccount.com`); that target SA must be the app/space member that sends messages.
- Grant the runtime SA `roles/iam.serviceAccountTokenCreator` **on the target Chat SA**.
- Enable `iamcredentials.googleapis.com`.
- The metadata base token must carry `cloud-platform` (or `.../auth/iam`) scope. A default-scope GCE VM returns `403 PERMISSION_DENIED: "Request had insufficient authentication scopes."`; match *scopes* to distinguish it from a missing role. GCE access scopes cannot be changed while the VM is running: create the VM with `--scopes=cloud-platform`, or use `gcloud compute instances set-scopes` followed by a stop/start.

`chat.bot` is a Workspace scope and is not a subset of `cloud-platform`, so the runtime metadata token cannot call Chat directly. The supported flow is runtime SA → distinct target Chat SA via `generateAccessToken`.

```bash
export GOOGLE_CHAT_ENABLED=true
export GOOGLE_CHAT_USE_ADC=true
export GOOGLE_CHAT_ADC_TARGET_SERVICE_ACCOUNT="openab-chat@PROJECT.iam.gserviceaccount.com"
```

For GKE + Helm, bind a Kubernetes ServiceAccount (KSA) to the **runtime** GSA
out of band, then attach that KSA to the gateway pod. The chart references an
existing KSA; it does not create or annotate one:

```yaml
agents:
  kiro:
    gateway:
      enabled: true
      serviceAccountName: openab-googlechat-runtime
      googleChat:
        useAdc: true
        adcTargetServiceAccount: openab-chat@PROJECT.iam.gserviceaccount.com
```

The `openab-googlechat-runtime` KSA must carry the usual
`iam.gke.io/gcp-service-account: openab-runtime@PROJECT.iam.gserviceaccount.com`
annotation and Workload Identity IAM binding. If `gateway.serviceAccountName`
is empty, the chart falls back to `agents.<name>.serviceAccountName`, then the
chart-global `serviceAccountName`.

Precedence: if a configured SA key loads successfully, it wins and ADC is ignored. If a key is configured but fails to load, the adapter uses the configured ADC target and logs the identity switch. `GOOGLE_CHAT_USE_ADC=true` without `GOOGLE_CHAT_ADC_TARGET_SERVICE_ACCOUNT` fails closed (ADC is not installed). If ADC fails and a static token is explicitly configured, the adapter degrades to it with a warning that it may represent a different identity.

> **Migrating an existing release from a SA key to ADC:** the chart renders the Google Chat Secret only when `saKeyJson` / `accessToken` is set, and that Secret carries `helm.sh/resource-policy: keep`. Switching to ADC-only stops Helm from managing it but **leaves the old key material in the cluster indefinitely**. Delete the orphaned Secret after the switch: it is the gateway Secret named by the chart's `openab.agentFullname` helper — `<release>-<agent>-gateway` by default (or the agent's `nameOverride`) — and it contains the `google-chat-sa-key-json` key. Find it with `kubectl get secrets -o name | grep gateway`, confirm with `kubectl get secret <name> -o jsonpath='{.data}' | grep -o google-chat-sa-key-json`, then `kubectl delete secret <name>`. Otherwise the "no key to mount or leak" benefit is undercut.

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
```

### `[googlechat]` Section (credentials + trust)

Since #1379 the `[googlechat]` section carries the full adapter configuration — config-first with `GOOGLE_CHAT_*` env fallback:

```toml
[googlechat]
enabled     = true
sa_key_json = "${GOOGLE_CHAT_SA_KEY_JSON}"
audience    = "projects/<project-number>/..."   # enables webhook JWT verification (L1)
allowed_users = ["users/123456789"]
```

### User Trust (`[googlechat]` section)

> **Trust resolution:** the `[googlechat]` section's trust settings apply in **both** deployment modes (enable the embedded adapter with `[googlechat] enabled = true`; `GOOGLE_CHAT_ENABLED=true` remains the env-only fallback). Broker-side enforcement goes through the shared per-platform trust registry with precedence `GATEWAY_*` env < `[gateway]` section < `[googlechat]` section — in the standalone-gateway mode, the broker's WebSocket path consults the same registry, so a `[googlechat]` section overrides `[gateway].allow_all_users` / `allowed_users` for this platform.

Identity trust defaults to **deny-all** (identity-trust-none ADR): unknown senders are rejected until explicitly admitted. Configure trust with a first-class `[googlechat]` section:

```toml
[googlechat]
allowed_users = ["users/123456789"]  # Chat user resource names (users/<id>)
# allow_all_users = true   # explicit opt-in only — any user can drive the agent
```

Each field falls back to its `GOOGLE_CHAT_ALLOW_ALL_USERS` / `GOOGLE_CHAT_ALLOWED_USERS` env var when unset.

> ⚠️ **Deprecated:** driving Google Chat trust through the uniform `GATEWAY_ALLOW_ALL_USERS` / `GATEWAY_ALLOWED_USERS` env vars still works but logs a startup warning; it will become a startup error in a later phase. Migrate to `[googlechat]` (or `GOOGLE_CHAT_*` env vars).

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
- **Send-once (no streaming)** — Google Chat is a request/response REST surface, so the adapter posts the full reply once with no in-place editing. It is in `NON_STREAMING_PLATFORMS`; see `docs/platforms/schema/googlechat.toml` for why (the unified adapter's synthetic message id is not a valid resource name → `patch` returns `400 INVALID_ARGUMENT`, and the API documents a 1 write/sec-per-space quota).
- **Inbound attachments** — image, text file, and audio attachments are downloaded via Google Chat Media API and stored to `~/.openab/media/inbound/<uuid>` (colocate filesystem store):
  - Images: resized to ≤1200px JPEG (q75); GIFs preserved. Max 10 MB.
  - Text files: only known text extensions (`.txt`, `.md`, `.json`, `.py`, `.rs`, etc.). Max 512 KB.
  - Audio: forwarded as-is for STT processing by core. Max 25 MB.
  - Drive-sourced attachments are skipped (require separate Drive API integration).

### Not Supported

- **Reactions** — Google Chat API does not support message reactions on behalf of bots
- **Outbound attachments** — bot cannot send image/file attachments back to the user yet
- **Drive-linked attachments** — only `UPLOADED_CONTENT` source is handled; `DRIVE_FILE` source skipped

## Environment Variables (Gateway)

| Variable | Required | Default | Description |
|---|---|---|---|
| `GOOGLE_CHAT_ENABLED` | Yes | `false` | Set to `true` or `1` to enable the adapter |
| `GOOGLE_CHAT_AUDIENCE` | Recommended | — | JWT audience for webhook verification — set to your full webhook URL (e.g. `https://your-domain.com/webhook/googlechat`) |
| `GOOGLE_CHAT_SA_KEY_JSON` | No | — | Service account key JSON string (enables auto-refresh) |
| `GOOGLE_CHAT_SA_KEY_FILE` | No | — | Path to service account key JSON file (alternative to `SA_KEY_JSON`) |
| `GOOGLE_CHAT_ACCESS_TOKEN` | No | — | Static OAuth2 access token (fallback, expires in 1 hour) |
| `GOOGLE_CHAT_USE_ADC` | No | `false` | Set to `true` or `1` to enable keyless ADC: attached runtime SA impersonates a distinct Chat-app target via IAM Credentials — see Option C |
| `GOOGLE_CHAT_ADC_TARGET_SERVICE_ACCOUNT` | With ADC | — | Email of the dedicated Chat-app SA to impersonate. Required when `USE_ADC=true`; MUST differ from the runtime SA |
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

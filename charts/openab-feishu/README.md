# openab-feishu

OpenAB + Feishu/Lark — single-pod Helm chart for deploying an AI agent on Feishu (飛書) or Lark.

## Architecture

```
┌──────────── Pod: openab-feishu ────────────┐
│                                             │
│  ┌──────────┐  localhost   ┌─────────────┐ │
│  │  openab  │ ◄──────────► │   gateway   │ │
│  │ (agent)  │              │  (feishu)   │ │
│  └──────────┘              └──────┬──────┘ │
│       │                           │        │
│       ▼                           ▼        │
│   PVC /home/agent      Outbound WebSocket  │
│                          to open.feishu.cn │
└─────────────────────────────────────────────┘
```

**Default mode: WebSocket** — gateway connects outbound to Feishu, no public endpoint needed.

Optional webhook mode adds a cloudflared sidecar (same pattern as `openab-telegram`).

## Quick Start

```bash
helm install my-bot ./charts/openab-feishu \
  --set feishu.appId="cli_xxx" \
  --set feishu.appSecret="xxx" \
  --namespace openab --create-namespace
```

Only **2 required parameters**. Everything else has sane defaults.

## Prerequisites: Feishu Open Platform Setup

Before deploying, create a Feishu app. This is a one-time setup.

> **Hermes Agent** and **OpenClaw** both support Feishu via their own gateway implementations (env-var based config, `pip install` / `npm install`). OAB's approach is K8s-native: a single `helm install` deploys everything, with credentials managed as K8s Secrets.

### 1. Create App

1. Go to [Feishu Open Platform](https://open.feishu.cn) (or [Lark Developer](https://open.larksuite.com) for overseas)
2. Click **Create Custom App**
3. Note down the **App ID** (`cli_xxx`) and **App Secret**

### 2. Enable Bot Capability

1. In the left sidebar, go to **App Features** → **Bot**
2. Toggle **Enable Bot** to ON

> ⚠️ This step is easy to miss. Without it, the app cannot receive messages.

### 3. Configure Event Subscription

1. Navigate to **Event Subscriptions** in the left sidebar
2. **Connection mode**: Select **WebSocket** (recommended)
   - WebSocket requires no public URL — the gateway connects outbound
   - If you must use webhook mode, see [Webhook Mode](#webhook-mode) below
3. **Add event**: `im.message.receive_v1` (Receive messages)

### 4. Add Permissions

Under **Permissions & Scopes**, add these scopes:

| Scope | Purpose |
|-------|---------|
| `im:message` | Send and receive messages |
| `im:message.group_at_msg` | Receive @mention messages in groups |
| `im:message.group_at_msg:readonly` | Read group @mention messages |
| `im:message.p2p_msg:readonly` | Read DM messages |
| `im:resource` | Download images/files from messages |
| `contact:user.base:readonly` | Resolve user display names |

### 5. Publish

Click **Create Version** → **Apply for publish**. For development, you can use the app in test mode without full approval.

### 6. Get IDs for Access Control (Optional)

To restrict which users/groups can interact with the bot:

- **Group ID** (`oc_xxx`): Open the group → click top-right menu → Settings → Group ID
- **User Open ID** (`ou_xxx`): Check gateway logs after the user sends a message, or use the Feishu Contact API

## Credential Management

Three options from simplest to most secure:

| # | Method | Security | Notes |
|---|--------|----------|-------|
| 1 | `--set feishu.appId=X --set feishu.appSecret=Y` | ⚠️ Stored in Helm release | Good for dev/testing |
| 2 | `kubectl create secret` + `--set existingSecret=name` | ✅ Out of Helm values | Good for production |
| 3 | `kubectl create secret --from-env-file=<(vault/aws sm)` + `--set existingSecret=name` | ✅✅ Never touches disk | Best for security |

### Option 2 example:

```bash
kubectl create secret generic feishu-creds -n openab \
  --from-literal=feishu-app-id="cli_xxx" \
  --from-literal=feishu-app-secret="xxx"

helm install my-bot ./charts/openab-feishu \
  --set existingSecret=feishu-creds \
  --namespace openab --create-namespace
```

### Option 3 example (AWS Secrets Manager):

```bash
kubectl create secret generic feishu-creds -n openab \
  --from-env-file=<(aws secretsmanager get-secret-value \
    --secret-id oab-feishu --query SecretString --output text | \
    jq -r '{"feishu-app-id": .appId, "feishu-app-secret": .appSecret} | to_entries[] | "\(.key)=\(.value)"')

helm install my-bot ./charts/openab-feishu \
  --set existingSecret=feishu-creds \
  --namespace openab --create-namespace
```

## Release Channel

| `channel` | Core image tag | Gateway image tag |
|-----------|---------------|-------------------|
| `stable` (default) | `ghcr.io/openabdev/openab:stable` | `v0.5.1` (pinned) |
| `beta` | `ghcr.io/openabdev/openab:beta` | `v0.5.1` (pinned) |

## Webhook Mode

If WebSocket is not available (e.g., network policy blocks outbound WebSocket), switch to webhook mode:

```bash
helm install my-bot ./charts/openab-feishu \
  --set feishu.appId="cli_xxx" \
  --set feishu.appSecret="xxx" \
  --set feishu.connectionMode="webhook" \
  --set feishu.verificationToken="xxx" \
  --set feishu.encryptKey="xxx" \
  --set tunnel.token="eyJ..." \
  --set webhookDomain="bot.example.com" \
  --namespace openab --create-namespace
```

This adds a cloudflared sidecar (3-container pod, same as `openab-telegram`).

After deployment:
1. Configure Cloudflare Tunnel ingress to point your domain at `localhost:8080`
2. In Feishu Open Platform → Event Subscriptions → set Request URL to `https://bot.example.com/webhook/feishu`
   - ⚠️ The gateway must be running when you set the URL — Feishu sends a challenge request immediately

## Lark (Overseas)

For Lark (larksuite.com) instead of Feishu (feishu.cn):

```bash
helm install my-bot ./charts/openab-feishu \
  --set feishu.appId="cli_xxx" \
  --set feishu.appSecret="xxx" \
  --set feishu.domain="lark" \
  --namespace openab --create-namespace
```

## Comparison with Other Platforms

| Feature | openab-feishu | openab-telegram | OpenClaw | Hermes Agent |
|---------|--------------|-----------------|----------|--------------|
| Default containers | 2 (agent + gateway) | 3 (+ cloudflared) | N/A (no Helm) | N/A (no Helm) |
| Public endpoint needed | ❌ (WebSocket) | ✅ (webhook) | Varies | Varies |
| Feishu/Lark support | ✅ Native | ❌ | ❌ | ❌ |
| K8s-native deployment | ✅ Helm chart | ✅ Helm chart | ❌ docker-compose | ❌ pip install |
| Credential params | 2 (appId + appSecret) | 2 (botToken + tunnelToken) | N/A | N/A |

## Values Reference

| Key | Default | Description |
|-----|---------|-------------|
| `feishu.appId` | `""` | **(required)** Feishu App ID |
| `feishu.appSecret` | `""` | **(required)** Feishu App Secret |
| `feishu.domain` | `"feishu"` | `"feishu"` or `"lark"` |
| `feishu.connectionMode` | `"websocket"` | `"websocket"` or `"webhook"` |
| `feishu.verificationToken` | `""` | Webhook verification token |
| `feishu.encryptKey` | `""` | Webhook encrypt key |
| `existingSecret` | `""` | Use pre-existing K8s Secret |
| `tunnel.enabled` | `false` | Enable cloudflared sidecar |
| `tunnel.token` | `""` | Cloudflare Tunnel token |
| `webhookDomain` | `""` | Domain for webhook URL |
| `channel` | `"stable"` | `"stable"` or `"beta"` |
| `platform.requireMention` | `true` | Require @mention in groups |
| `platform.allowedGroups` | `[]` | Allowed group chat IDs |
| `platform.allowedUsers` | `[]` | Allowed user open_ids |
| `persistence.enabled` | `true` | Enable PVC for agent state |
| `persistence.size` | `"1Gi"` | PVC size |

## Troubleshooting

| Problem | Fix |
|---------|-----|
| Bot doesn't respond to DMs | Ensure Bot capability is enabled in App Features → Bot |
| Bot doesn't respond in groups | Ensure you @mention the bot (default: `requireMention: true`) |
| Bot doesn't receive any messages | Check event subscription: must have `im.message.receive_v1` and WebSocket mode selected |
| Gateway logs show "token refresh error" | Verify `appId` and `appSecret` are correct |
| Gateway logs show "feishu ws endpoint error" | WebSocket mode requires the app to be published (at least test version) |
| Permission denied on image/file download | Grant `im:resource` scope and re-publish the app |
| User names show as `ou_xxx` | Grant `contact:user.base:readonly` scope |
| Pod CrashLoopBackOff | Check `kubectl logs -c gateway` — usually a credential issue |

## Comparison: OAB vs OpenClaw vs Hermes Agent (Feishu)

| Aspect | OAB (this chart) | OpenClaw | Hermes Agent |
|--------|-----------------|----------|--------------|
| Deployment | `helm install` (K8s-native) | `npx @larksuite/openclaw-lark install` | `hermes gateway setup` |
| Runtime | Rust binary (gateway) + any agent | Node.js | Python |
| Connection mode | WebSocket (default) / Webhook | WebSocket (default) / Webhook | WebSocket (default) / Webhook |
| Config style | Helm values + K8s Secrets | JSON config file | `.env` + `config.yaml` |
| Credential management | 3-tier (--set → K8s Secret → external SM) | Plain config file | `.env` file |
| Security hardening | Non-root, read-only rootfs, drop all caps | N/A (runs as user process) | N/A (runs as user process) |
| Public endpoint needed | ❌ (WebSocket mode) | ❌ (WebSocket mode) | ❌ (WebSocket mode) |
| Feishu-specific features | @mention gating, user allowlist, group allowlist, bot-to-bot, media proxy | Streaming cards, multi-account, ACP sessions, pairing | Interactive cards, document comments, per-group ACL, burst batching |


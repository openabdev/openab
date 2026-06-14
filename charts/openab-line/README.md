# openab-line

OpenAB + LINE in a single pod — OAB agent and gateway colocated, with optional Cloudflare Tunnel sidecar.

## Architecture

```
┌──────────────────────────────────────────────────────┐
│ Pod: openab-line                                     │
│                                                      │
│  ┌──────────┐  ws://localhost:8080/ws  ┌──────────┐  │
│  │  openab  │◄────────────────────────►│ gateway  │  │
│  │ (agent)  │                          │  :8080   │  │
│  └────┬─────┘                          └────┬─────┘  │
│       │                                      │        │
│       │ /home/agent (PVC, shared HOME)       │        │
│       │ ~/.openab/media/inbound/<uuid>       │        │
│       ▼                                      ▼        │
│   shared filesystem                    /webhook/line  │
│                                                      │
│  optional: cloudflared sidecar                       │
└──────────────────────────────────────────────────────┘
```

This chart follows OpenAB's current inbound-attachment model: the gateway stores media under the shared `$HOME`, and core reads `attachments[].path` from the same filesystem. That is why the agent and gateway run in the **same pod** and share the **same working directory / HOME**.

## Quick Start

### Option A: Cloudflare Tunnel

```bash
helm install my-bot ./charts/openab-line \
  --set line.channelSecret="xxx" \
  --set line.channelAccessToken="xxx" \
  --set tunnel.enabled=true \
  --set tunnel.token="eyJ..." \
  --set webhookDomain=bot.example.com \
  --namespace openab --create-namespace
```

### Option B: Existing Ingress / LoadBalancer

```bash
helm install my-bot ./charts/openab-line \
  --set line.channelSecret="xxx" \
  --set line.channelAccessToken="xxx" \
  --namespace openab --create-namespace
```

Then expose the generated Service (`ClusterIP` by default) at:

```text
https://YOUR_DOMAIN/webhook/line
```

## LINE Setup

### 1. Create a LINE Official Account

1. Go to [LINE Official Account Manager](https://manager.line.biz)
2. Create an account and enable **Messaging API**
3. Open the channel in [LINE Developers Console](https://developers.line.biz)

### 2. Get Credentials

- **Channel secret** → `line.channelSecret`
- **Channel access token** → `line.channelAccessToken`

### 3. Configure the Webhook

In LINE Developers Console → **Messaging API**:

1. Set **Webhook URL** to `https://YOUR_DOMAIN/webhook/line`
2. Turn **Use webhook** ON
3. Turn **Auto-reply messages** OFF
4. Click **Verify**

## Credentials

Three options from simplest to most secure:

| # | Method | Security | Notes |
|---|--------|----------|-------|
| 1 | `--set line.channelSecret=... --set line.channelAccessToken=...` | ⚠️ Stored in Helm release | Good for dev/testing |
| 2 | `kubectl create secret` + `--set existingSecret=name` | ✅ Out of Helm values | Good for production |
| 3 | External secret manager → K8s Secret → `existingSecret` | ✅✅ Best for production | Recommended |

### Option 2 example

```bash
kubectl create secret generic line-creds -n openab \
  --from-literal=line-channel-secret="xxx" \
  --from-literal=line-channel-access-token="xxx"

helm install my-bot ./charts/openab-line \
  --set existingSecret=line-creds \
  --namespace openab --create-namespace
```

If you enable the tunnel sidecar, add:

```bash
  --from-literal=cloudflare-tunnel-token="eyJ..."
```

## Agent Images

The chart keeps the agent side generic. You can swap the image/command/args, for example:

```bash
helm upgrade --install my-bot ./charts/openab-line \
  --set image.repository=ghcr.io/openabdev/openab-copilot \
  --set-string agent.command=copilot \
  --set agent.args[0]=--acp \
  --set agent.args[1]=--stdio \
  --set agent.workingDir=/home/node \
  --set existingSecret=line-creds \
  --namespace openab
```

## LINE-specific Notes

- LINE is **webhook-only**. This chart always creates a Service; either expose it yourself or enable `tunnel.enabled=true`.
- LINE replies use Reply API first, then fall back to Push API when the reply token expires. Slow coding-agent responses can therefore consume push quota.
- LINE reactions are not supported, so this chart defaults `agent.reactions.enabled=false`.
- LINE-hosted inbound images are supported. `contentProvider.type = "external"` images are still skipped.

## Values Reference

| Key | Default | Description |
|-----|---------|-------------|
| `line.channelSecret` | `""` | **(required)** LINE channel secret |
| `line.channelAccessToken` | `""` | **(required)** LINE channel access token |
| `existingSecret` | `""` | Use a pre-existing Secret instead of creating one |
| `channel` | `stable` | Agent release channel (`stable` or `beta`) |
| `tunnel.enabled` | `false` | Enable cloudflared sidecar |
| `tunnel.token` | `""` | Cloudflare Tunnel token (required when `tunnel.enabled=true`) |
| `tunnel.image` | `cloudflare/cloudflared` | Cloudflared image repository |
| `tunnel.tag` | `2026.5.0` | Cloudflared image tag |
| `webhookDomain` | `""` | Domain shown in post-install notes |
| `image.repository` | `ghcr.io/openabdev/openab` | Agent image repository |
| `image.tag` | `""` | Agent image tag (defaults to `channel` value) |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |
| `gateway.image` | `ghcr.io/openabdev/openab-gateway` | Gateway image repository (override for air-gapped / private-registry installs) |
| `gateway.tag` | `v0.5.3` | Gateway image tag. Pinned to version tested with this chart — change with care |
| `agent.command` | `kiro-cli` | Agent entrypoint command |
| `agent.args` | `["acp","--trust-all-tools"]` | Agent command arguments |
| `agent.workingDir` | `/home/agent` | Shared HOME and PVC/emptyDir mount path |
| `agent.env` | `{}` | Extra environment variables (map) |
| `agent.envFrom` | `[]` | Extra envFrom sources (ConfigMaps/Secrets) |
| `agent.secretEnv` | `[]` | Extra environment variables from Secrets |
| `agent.pool.maxSessions` | `10` | Max concurrent sessions |
| `agent.pool.sessionTtlHours` | `24` | Session TTL in hours |
| `agent.reactions.enabled` | `false` | Enable reaction events (LINE does not support reactions) |
| `agent.reactions.removeAfterReply` | `false` | Remove reaction after agent replies |
| `platform.allowAllUsers` | `null` | Override allow-all-users (`null` = defer to `allowedUsers` list) |
| `platform.allowAllChannels` | `null` | Override allow-all-channels (`null` = defer to `allowedChannels` list) |
| `platform.allowedUsers` | `[]` | Allowed LINE user IDs (`Uxxx…`). Empty = allow all when `allowAllUsers` is null |
| `platform.allowedChannels` | `[]` | Allowed LINE chat/group IDs. Empty = allow all when `allowAllChannels` is null |
| `persistence.enabled` | `true` | Enable PVC for agent state and media |
| `persistence.existingClaim` | `""` | Use an existing PVC instead of creating one |
| `persistence.storageClass` | `""` | Storage class (`""` = cluster default) |
| `persistence.size` | `1Gi` | PVC size |
| `service.type` | `ClusterIP` | Service type |
| `service.port` | `8080` | Service port |
| `service.annotations` | `{}` | Extra annotations for the Service |
| `resources` | `{}` | Container resource requests/limits |
| `nodeSelector` | `{}` | Node selector |
| `tolerations` | `[]` | Tolerations |
| `affinity` | `{}` | Affinity rules |

## Troubleshooting

| Problem | Fix |
|---------|-----|
| Webhook verify fails | Ensure your public endpoint reaches the Service on port 8080 and the URL ends with `/webhook/line` |
| Gateway logs show invalid signature | Re-check `line-channel-secret` |
| Bot can reply to text but not see images | Verify the agent and gateway share the same `HOME` and PVC; this chart does by default |
| Bot does not respond at all | Confirm **Use webhook** is ON and **Auto-reply messages** is OFF in LINE Developers Console |
| Authenticated CLI lost session after restart | Keep `persistence.enabled=true` or mount an existing claim |

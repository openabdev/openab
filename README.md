# openab Helm Chart

A Helm chart for deploying [openab](https://github.com/openabdev/openab) — a lightweight, secure, cloud-native ACP harness that bridges Discord and any ACP-compatible coding CLI (Kiro CLI, Claude Code, Codex, Gemini, etc.).

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│    openab    │──────────────►│  coding CLI  │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│  (acp mode)  │
└──────────────┘               └──────────────┘               └──────────────┘
```

## Prerequisites

- Kubernetes 1.21+
- Helm 3.0+
- A Discord bot token ([setup guide](https://github.com/openabdev/openab/blob/main/docs/discord-bot-howto.md))

## Installation

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update
```

```bash
# Kiro CLI (single agent)
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

> ⚠️ Always use `--set-string` for channel IDs to avoid float64 precision loss.

## Multi-Agent

One Helm release can run multiple agents simultaneously — each gets its own Deployment, ConfigMap, Secret, and PVC.

```bash
# Codex
helm install openab openab/openab \
  --set agents.codex.command=codex-acp \
  --set agents.codex.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.codex.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Claude Code
helm install openab openab/openab \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Gemini
helm install openab openab/openab \
  --set agents.gemini.command=gemini \
  --set 'agents.gemini.args[0]=--acp' \
  --set agents.gemini.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.gemini.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

## Upgrade

```bash
helm upgrade openab openab/openab -f my-values.yaml
```

## Values Reference

Each agent is configured under `agents.<name>`:

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/openabdev/openab` | Container image repository |
| `image.tag` | `""` | Container image tag |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |
| `agents.<name>.command` | `kiro-cli` | CLI command to run as agent |
| `agents.<name>.args` | `["acp", "--trust-all-tools"]` | Arguments passed to the agent CLI |
| `agents.<name>.workingDir` | `/home/agent` | Working directory for the agent process |
| `agents.<name>.discord.botToken` | `""` | Discord bot token |
| `agents.<name>.discord.allowedChannels` | `[]` | List of Discord channel IDs |
| `agents.<name>.env` | `{}` | Extra environment variables for the agent |
| `agents.<name>.envFrom` | `[]` | Extra envFrom sources (ConfigMap / Secret refs) |
| `agents.<name>.pool.maxSessions` | `10` | Maximum concurrent sessions |
| `agents.<name>.pool.sessionTtlHours` | `24` | Idle session TTL in hours |
| `agents.<name>.reactions.enabled` | `true` | Enable emoji status reactions |
| `agents.<name>.reactions.removeAfterReply` | `false` | Remove reactions after bot replies |
| `agents.<name>.persistence.enabled` | `true` | Enable PVC for auth token persistence |
| `agents.<name>.persistence.storageClass` | `""` | Storage class (empty = cluster default) |
| `agents.<name>.persistence.size` | `1Gi` | PVC size |
| `agents.<name>.agentsMd` | `""` | Content injected as `/home/agent/AGENTS.md` |
| `agents.<name>.resources` | `{}` | Container resource requests/limits |
| `agents.<name>.nodeSelector` | `{}` | Node selector |
| `agents.<name>.tolerations` | `[]` | Tolerations |
| `agents.<name>.affinity` | `{}` | Affinity rules |

## Example values.yaml

```yaml
agents:
  kiro:
    command: kiro-cli
    args: [acp, --trust-all-tools]
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "1234567890123456789"
    workingDir: /home/agent
    pool:
      maxSessions: 10
      sessionTtlHours: 24
    reactions:
      enabled: true
      removeAfterReply: false
    persistence:
      enabled: true
      storageClass: ""
      size: 1Gi
    agentsMd: |
      IDENTITY - your agent identity
      SOUL - your agent personality
      USER - how agent should address the user
```

## Multi-Agent Example (values.yaml)

```yaml
agents:
  kiro:
    command: kiro-cli
    args: [acp, --trust-all-tools]
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels: ["YOUR_KIRO_CHANNEL_ID"]
    persistence:
      enabled: true
  claude:
    command: claude-agent-acp
    args: []
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels: ["YOUR_CLAUDE_CHANNEL_ID"]
    persistence:
      enabled: true
```

## Post-Install: Authenticate

Each agent requires a one-time auth. The PVC persists tokens across pod restarts.

```bash
# Kiro CLI
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow

# Codex
kubectl exec -it deployment/openab-codex -- codex login --device-auth

# Claude Code
kubectl exec -it deployment/openab-claude -- claude setup-token
# Then: helm upgrade openab openab/openab --set agents.claude.env.CLAUDE_CODE_OAUTH_TOKEN="<token>"

# Gemini
kubectl exec -it deployment/openab-gemini -- gemini
# Or: helm upgrade openab openab/openab --set agents.gemini.env.GEMINI_API_KEY="<key>"
```

Restart after auth:

```bash
kubectl rollout restart deployment/openab-<agent>
```

## Uninstall

```bash
helm uninstall openab
```

> **Note:** Secrets with `helm.sh/resource-policy: keep` and PVCs are not deleted automatically. To remove them:
> ```bash
> kubectl delete secret openab-kiro
> kubectl delete pvc openab-kiro
> ```

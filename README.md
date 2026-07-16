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
- A Discord bot token ([setup guide](https://github.com/openabdev/openab/blob/main/docs/discord.md))

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

## Single Agent (non-default)

To deploy only one non-default agent, disable the default `kiro` with `enabled: false`:

```bash
# Claude Code only
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:78f8d2c \
  --set agents.claude.workingDir=/home/node \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Codex only
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.codex.command=codex-acp \
  --set agents.codex.image=ghcr.io/openabdev/openab-codex:78f8d2c \
  --set agents.codex.workingDir=/home/node \
  --set agents.codex.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.codex.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Gemini only
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.gemini.command=gemini \
  --set 'agents.gemini.args={--acp}' \
  --set agents.gemini.image=ghcr.io/openabdev/openab-gemini:78f8d2c \
  --set agents.gemini.workingDir=/home/node \
  --set agents.gemini.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.gemini.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

> **Why `enabled: false`?** Helm deep-merges values — adding `agents.claude` does **not** remove the default `agents.kiro`. Without `enabled: false`, a broken kiro agent (no token, placeholder channel) would be deployed alongside your agent.

## Multi-Agent

One Helm release can run multiple agents simultaneously — each gets its own Deployment, ConfigMap, Secret, and PVC.

```bash
# Kiro + Claude in one release
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$KIRO_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=KIRO_CHANNEL_ID' \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:78f8d2c \
  --set agents.claude.workingDir=/home/node \
  --set agents.claude.discord.botToken="$CLAUDE_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=CLAUDE_CHANNEL_ID'
```

## Upgrade

```bash
helm upgrade openab openab/openab -f my-values.yaml
```

## Values Reference

Each agent is configured under `agents.<name>`:

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/openabdev/openab` | Default container image repository |
| `image.tag` | `""` (uses appVersion) | Default image tag, defaults to Chart appVersion |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |
| `agents.<name>.enabled` | `true` | Set to `false` to skip creating resources for this agent |
| `agents.<name>.image` | `""` | Override full image reference (e.g. `ghcr.io/openabdev/openab-claude:latest`), defaults to `image.repository:appVersion` |
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
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_CHANNEL_ID"
```

## Claude Only Example (values.yaml)

```yaml
agents:
  kiro:
    enabled: false
  claude:
    image: ghcr.io/openabdev/openab-claude:78f8d2c
    command: claude-agent-acp
    workingDir: /home/node
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_CHANNEL_ID"
```

## Codex Only Example (values.yaml)

```yaml
agents:
  kiro:
    enabled: false
  codex:
    image: ghcr.io/openabdev/openab-codex:78f8d2c
    command: codex-acp
    workingDir: /home/node
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_CHANNEL_ID"
```

## Gemini Only Example (values.yaml)

```yaml
agents:
  kiro:
    enabled: false
  gemini:
    image: ghcr.io/openabdev/openab-gemini:78f8d2c
    command: gemini
    args: ["--acp"]
    workingDir: /home/node
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_CHANNEL_ID"
    env:
      GEMINI_API_KEY: "${GEMINI_API_KEY}"
```

## Multi-Agent Example (values.yaml)

```yaml
agents:
  kiro:
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_KIRO_CHANNEL_ID"
  claude:
    image: ghcr.io/openabdev/openab-claude:78f8d2c
    command: claude-agent-acp
    workingDir: /home/node
    discord:
      botToken: ""  # set via --set or external secret
      allowedChannels:
        - "YOUR_CLAUDE_CHANNEL_ID"
```

## All Four Agents Example (values.yaml)

```yaml
agents:
  kiro:
    discord:
      botToken: ""
      allowedChannels:
        - "KIRO_CHANNEL_ID"
  claude:
    image: ghcr.io/openabdev/openab-claude:78f8d2c
    command: claude-agent-acp
    workingDir: /home/node
    discord:
      botToken: ""
      allowedChannels:
        - "CLAUDE_CHANNEL_ID"
  codex:
    image: ghcr.io/openabdev/openab-codex:78f8d2c
    command: codex-acp
    workingDir: /home/node
    discord:
      botToken: ""
      allowedChannels:
        - "CODEX_CHANNEL_ID"
  gemini:
    image: ghcr.io/openabdev/openab-gemini:78f8d2c
    command: gemini
    args: ["--acp"]
    workingDir: /home/node
    discord:
      botToken: ""
      allowedChannels:
        - "GEMINI_CHANNEL_ID"
    env:
      GEMINI_API_KEY: "${GEMINI_API_KEY}"
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

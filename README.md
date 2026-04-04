# agent-broker Helm Chart

A Helm chart for deploying [agent-broker](https://github.com/thepagent/agent-broker) — a Rust bridge service between Discord and any ACP-compatible coding CLI (Kiro CLI, Claude Code, Codex, Gemini, etc.).

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│ agent-broker │──────────────►│  coding CLI  │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│  (acp mode)  │
└──────────────┘               └──────────────┘               └──────────────┘
```

## Prerequisites

- Kubernetes 1.21+
- Helm 3.0+
- A Discord bot token ([setup guide](https://github.com/thepagent/agent-broker/blob/main/docs/discord-bot-howto.md))

## Installation

##### Helm Repository (GitHub Pages)

```bash
helm repo add agent-broker https://thepagent.github.io/agent-broker
helm repo update
```

```bash
helm install agent-broker agent-broker/agent-broker \
  --set discord.botToken="YOUR_BOT_TOKEN" \
  --set discord.allowedChannels[0]="YOUR_CHANNEL_ID"
```

##### OCI Registry

```bash
helm install agent-broker oci://ghcr.io/thepagent/agent-broker \
  --version 0.1.8 \
  --set discord.botToken="YOUR_BOT_TOKEN" \
  --set discord.allowedChannels[0]="YOUR_CHANNEL_ID"
```

##### Using a values file

```bash
helm install agent-broker agent-broker/agent-broker -f my-values.yaml
```

## Upgrade

```bash
helm upgrade agent-broker agent-broker/agent-broker -f my-values.yaml
```

Or with OCI:

```bash
helm upgrade agent-broker oci://ghcr.io/thepagent/agent-broker --version 0.1.8 -f my-values.yaml
```

## Values Reference

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/thepagent/agent-broker` | Container image repository |
| `image.tag` | `fea0445` | Container image tag |
| `image.pullPolicy` | `IfNotPresent` | Image pull policy |
| `replicas` | `1` | Number of replicas |
| `strategy.type` | `Recreate` | Deployment strategy |
| `discord.botToken` | `""` | Discord bot token (use `--set` or external secret) |
| `discord.allowedChannels` | `[]` | List of Discord channel IDs to listen on |
| `agent.command` | `kiro-cli` | CLI command to run as agent |
| `agent.args` | `["acp", "--trust-all-tools"]` | Arguments passed to the agent CLI |
| `agent.workingDir` | `/home/agent` | Working directory for the agent process |
| `agent.env` | `{}` | Extra environment variables passed to the agent |
| `pool.maxSessions` | `10` | Maximum concurrent sessions |
| `pool.sessionTtlHours` | `24` | Idle session TTL in hours |
| `reactions.enabled` | `true` | Enable emoji status reactions |
| `reactions.removeAfterReply` | `false` | Remove reactions after bot replies |
| `persistence.enabled` | `true` | Enable PVC for auth token persistence |
| `persistence.storageClass` | `""` | Storage class (empty = cluster default) |
| `persistence.size` | `1Gi` | PVC size |
| `agentsMd` | `""` | Content to inject as `/home/agent/AGENTS.md` |
| `resources` | `{}` | Container resource requests/limits |
| `env` | `{}` | Extra environment variables for the broker process |
| `envFrom` | `[]` | Extra envFrom sources (ConfigMap / Secret refs) |
| `nodeSelector` | `{}` | Node selector |
| `tolerations` | `[]` | Tolerations |
| `affinity` | `{}` | Affinity rules |

## Example values.yaml

```yaml
image:
  repository: ghcr.io/thepagent/agent-broker
  tag: "fea0445"

discord:
  botToken: ""  # set via --set or external secret
  allowedChannels:
    - "1234567890123456789"

agent:
  command: kiro-cli
  args:
    - acp
    - --trust-all-tools
  workingDir: /home/agent
  env: {}
    # ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY}"

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

# Optional: inject AGENTS.md into /home/agent/AGENTS.md
agentsMd: |
  IDENTITY - your agent identity
  SOUL - your agent personality
  USER - how agent should address the user
```

## Agent Backends

Swap the `agent` block to use any ACP-compatible CLI:

```yaml
# Kiro CLI (default)
agent:
  command: kiro-cli
  args: ["acp", "--trust-all-tools"]

# Claude Code
agent:
  command: claude
  args: ["--acp"]
  env:
    ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY}"

# Codex
agent:
  command: codex
  args: ["--acp"]
  env:
    OPENAI_API_KEY: "${OPENAI_API_KEY}"

# Gemini
agent:
  command: gemini
  args: ["--acp"]
  env:
    GEMINI_API_KEY: "${GEMINI_API_KEY}"
```

## Post-Install: Authenticate kiro-cli

kiro-cli requires a one-time OAuth login. The PVC persists tokens across pod restarts.

```bash
kubectl exec -it deployment/agent-broker -- kiro-cli login --use-device-flow
```

Follow the device code flow in your browser, then restart the pod:

```bash
kubectl rollout restart deployment/agent-broker
```

## Uninstall

```bash
helm uninstall agent-broker
```

> **Note:** The PVC is not deleted automatically. To remove it:
> ```bash
> kubectl delete pvc agent-broker
> ```

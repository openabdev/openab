# openab Helm Chart

This chart deploys one or more OpenAB agents on Kubernetes.

## Common Values

This page highlights commonly used values and deployment patterns. For the complete list of supported options and defaults, run `helm show values openab/openab` or inspect [`values.yaml`](values.yaml).

### Release naming

| Value | Description | Default |
|-------|-------------|---------|
| `nameOverride` | Override the chart name portion used in generated resource names. For per-agent resource names, use `agents.<name>.nameOverride`. | `""` |
| `fullnameOverride` | Override the full generated release name for chart resources. Useful when deploying multiple instances with predictable names. | `""` |

### Agent values

Each agent lives under `agents.<name>`.

| Value | Description | Default |
|-------|-------------|---------|
| `discord.botToken` | Discord bot token for the agent. | `""` |
| `discord.allowedChannels` | Channel allowlist. Use `--set-string` for Discord IDs. | `["YOUR_CHANNEL_ID"]` |
| `discord.allowedUsers` | User allowlist. Empty = allow all users by default. Use `--set-string` for Discord IDs. | `[]` |
| `discord.allowDm` | Whether the Discord bot responds to direct messages. | `false` |
| `discord.allowBotMessages` | Controls whether bot messages can trigger replies. | `"off"` |
| `discord.trustedBotIds` | Optional bot ID allowlist when bot-message replies are enabled. | `[]` |
| `slack.enabled` | Enable the Slack adapter for the agent. | `false` |
| `slack.botToken` | Slack Bot User OAuth token. | `""` |
| `slack.appToken` | Slack App-Level token for Socket Mode. | `""` |
| `slack.allowedChannels` | Slack channel allowlist. Empty means allow all channels by default. | `[]` |
| `slack.allowedUsers` | Slack user allowlist. Empty means allow all users by default. | `[]` |
| `nameOverride` | Override this agent's generated resource name. | `""` |
| `workingDir` | Working directory and HOME inside the container. | `"/home/agent"` |
| `env` | Inline environment variables passed to the agent process. | `{}` |
| `envFrom` | Additional environment sources from existing Secrets or ConfigMaps. | `[]` |
| `pool.maxSessions` | Maximum concurrent ACP sessions for the agent. | `10` |
| `pool.sessionTtlHours` | Idle session TTL in hours. | `24` |
| `reactions.enabled` | Enable status reactions. | `true` |
| `reactions.removeAfterReply` | Remove status reactions after the agent replies. | `false` |
| `reactions.toolDisplay` | Tool display verbosity: `full`, `compact`, or `none`. | `"full"` |
| `stt.enabled` | Enable voice-message speech-to-text. | `false` |
| `stt.apiKey` | API key for the speech-to-text provider. | `""` |
| `stt.model` | STT model name. | `"whisper-large-v3-turbo"` |
| `stt.baseUrl` | STT API base URL. | `"https://api.groq.com/openai/v1"` |
| `gateway.enabled` | Enable the gateway config block for webhook-based platforms. | `false` |
| `gateway.deploy` | Deploy the gateway Deployment and Service. | `true` |
| `cron.usercronEnabled` | Enable user-provided cron configuration. | `false` |
| `cronjobs` | Config-driven scheduled messages for an agent. | `[]` |
| `persistence.enabled` | Enable persistent storage for auth and settings. | `true` |
| `persistence.existingClaim` | Reuse an existing PVC instead of creating one. | `""` |
| `agentsMd` | Contents of `AGENTS.md` mounted into the working directory. | `""` |
| `extraInitContainers` | Additional init containers for the agent pod. | `[]` |
| `extraContainers` | Additional sidecar containers for the agent pod. | `[]` |
| `extraVolumeMounts` | Additional volume mounts for the main agent container. | `[]` |
| `extraVolumes` | Additional volumes for the agent pod. | `[]` |

## Examples

### Override generated names

```bash
helm install prod openab/openab \
  --set fullnameOverride=my-openab \
  --set-literal agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

This makes generated resource names use `my-openab` (for example `my-openab-kiro`) instead of the default `prod-openab`.

### Load credentials with `envFrom`

```yaml
agents:
  kiro:
    envFrom:
      - secretRef:
          name: openab-agent-secrets
      - configMapRef:
          name: openab-agent-config
```

This is useful for credentials such as `GH_TOKEN` without storing them directly in Helm values.

### Provide `AGENTS.md` with `--set-file`

```bash
helm install openab openab/openab \
  --set-literal agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set-file agents.kiro.agentsMd=./AGENTS.md
```

### Discord ID precision warning

Discord IDs must be set with `--set-string`, not `--set`. Otherwise Helm may coerce them into numbers and lose precision.

# openab Helm Chart

This chart deploys one or more OpenAB agents on Kubernetes.

## Values Reference

### Release naming

| Value | Description | Default |
|-------|-------------|---------|
| `nameOverride` | Override the chart name portion used in generated resource names. | `""` |
| `fullnameOverride` | Override the full generated release name for chart resources. Useful when deploying multiple instances with predictable names. | `""` |

### Agent values

Each agent lives under `agents.<name>`.

| Value | Description | Default |
|-------|-------------|---------|
| `discord.botToken` | Discord bot token for the agent. | `""` |
| `discord.allowedChannels` | Channel allowlist. Use `--set-string` for Discord IDs. | `["YOUR_CHANNEL_ID"]` |
| `discord.allowedUsers` | User allowlist. Empty means allow all users. Use `--set-string` for Discord IDs. | `[]` |
| `workingDir` | Working directory and HOME inside the container. | `"/home/agent"` |
| `env` | Inline environment variables passed to the agent process. | `{}` |
| `envFrom` | Additional environment sources from existing Secrets or ConfigMaps. | `[]` |
| `agentsMd` | Contents of `AGENTS.md` mounted into the working directory. | `""` |

## Examples

### Override generated names

```bash
helm install prod openab/openab \
  --set fullnameOverride=my-openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
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
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set-file agents.kiro.agentsMd=./AGENTS.md
```

### Discord ID precision warning

Discord IDs must be set with `--set-string`, not `--set`. Otherwise Helm may coerce them into numbers and lose precision.

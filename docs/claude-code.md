# Claude Code

Claude Code uses the [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) adapter for ACP support.

## Docker Image

```bash
docker build -f Dockerfile.claude -t openab-claude:latest .
```

The image installs `@agentclientprotocol/claude-agent-acp` and `@anthropic-ai/claude-code` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:latest \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
command = "claude-agent-acp"
args = []
working_dir = "/home/node"
```

## Authentication

Sign in interactively using the OAuth device flow. Credentials are stored on disk (persisted via PVC across pod restarts):

```bash
kubectl exec -it deployment/openab-claude -- claude auth login
```

After authenticating, restart the pod so the bot process loads the new credentials:

```bash
kubectl rollout restart deployment/openab-claude
```

> **Note:** `claude setup-token` is a different command — it generates a long-lived token for CI/scripts and prints it without saving locally. For container-based deployments, `claude auth login` is the correct approach as it persists credentials to the filesystem.

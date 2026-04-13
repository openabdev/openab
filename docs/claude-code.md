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

```bash
kubectl exec -it deployment/openab-claude -- claude setup-token
```

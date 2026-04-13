# Codex

Codex uses the [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) adapter for ACP support.

## Docker Image

```bash
docker build -f Dockerfile.codex -t openab-codex:latest .
```

The image installs `@zed-industries/codex-acp` and `@openai/codex` globally via npm.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.codex.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.codex.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.codex.image=ghcr.io/openabdev/openab-codex:latest \
  --set agents.codex.command=codex-acp \
  --set agents.codex.workingDir=/home/node
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
command = "codex-acp"
args = []
working_dir = "/home/node"
```

## Authentication

```bash
kubectl exec -it deployment/openab-codex -- codex login --device-auth
```

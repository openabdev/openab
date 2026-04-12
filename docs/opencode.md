# OpenCode

OpenCode supports ACP natively via the `acp` subcommand — no adapter needed.

## Docker Image

```bash
docker build -f Dockerfile.opencode -t openab-opencode:latest .
```

The image installs `opencode-ai` globally via npm on `node:22-bookworm-slim`.

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.opencode.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.opencode.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.opencode.image=ghcr.io/openabdev/openab-opencode:latest \
  --set agents.opencode.command=opencode \
  --set 'agents.opencode.args={acp}' \
  --set agents.opencode.workingDir=/home/node
```

> Set `agents.kiro.enabled=false` to disable the default Kiro agent.

## Manual config.toml

```toml
[agent]
command = "opencode"
args = ["acp"]
working_dir = "/home/node"
```

## Authentication

```bash
kubectl exec -it deployment/openab-opencode -- opencode auth login
```

Follow the browser OAuth flow, then restart the pod:

```bash
kubectl rollout restart deployment/openab-opencode
```

## Notes

- **Tool authorization**: OpenCode handles tool authorization internally and never emits `session/request_permission` — all tools run without user confirmation, equivalent to `--trust-all-tools` on other backends.
- **Frequent releases**: OpenCode releases very frequently (often daily). The pinned version in `Dockerfile.opencode` should be bumped via a dedicated PR when an update is needed.

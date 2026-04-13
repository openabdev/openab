# Kiro CLI (Default Agent)

Kiro CLI is the default agent backend for OpenAB. It supports ACP natively — no adapter needed.

## Docker Image

The default `Dockerfile` bundles both `openab` and `kiro-cli`:

```bash
docker build -t openab:latest .
```

## Helm Install

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update

helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

## Manual config.toml

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"
```

## Authentication

Kiro CLI requires a one-time OAuth login. The PVC persists tokens across pod restarts.

```bash
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow
```

Follow the device code flow in your browser, then restart the pod:

```bash
kubectl rollout restart deployment/openab-kiro
```

### Persisted Paths (PVC)

| Path | Contents |
|------|----------|
| `~/.kiro/` | Settings, skills, sessions |
| `~/.local/share/kiro-cli/` | OAuth tokens (`data.sqlite3` → `auth_kv` table), conversation history |

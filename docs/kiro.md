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

## Slash Commands

| Command | Purpose | Status |
|---------|---------|--------|
| `/models` | Switch AI model | ✅ Implemented |
| `/agents` | Switch agent mode | ✅ Implemented |
| `/cancel` | Cancel current generation | ✅ Implemented |

### `/models` — Switch AI Model

Kiro CLI returns available models via ACP `configOptions` (category: `"model"`) on session creation. User types `/models` in a thread → select menu appears → pick a model → OpenAB sends `session/set_config_option` (falls back to `/model <value>` prompt if not supported).

### `/agents` — Switch Agent Mode

Same mechanism as `/models` but for the `agent` category. Kiro CLI exposes modes like `kiro_default` and `kiro_planner` via `configOptions`.

### `/cancel` — Cancel Current Operation

Sends a `session/cancel` JSON-RPC notification to abort in-flight LLM requests and tool calls. Works immediately — no need to wait for the current response to finish.

**Note:** All slash commands only work in threads where a conversation is already active. If no session exists, they will prompt the user to start one first.

See [docs/slash-commands.md](slash-commands.md) for full details.

## Built-in Kiro CLI Commands

All built-in kiro-cli slash commands can be passed directly after an @mention:

```
@MyBot /compact
@MyBot /clear
@MyBot /model claude-sonnet-4
```

These are forwarded as-is to the kiro-cli ACP session as a prompt. Any command that kiro-cli supports in its interactive mode works here.

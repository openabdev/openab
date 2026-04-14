# OpenAB — Open Agent Broker

A lightweight, secure, cloud-native ACP harness that bridges Discord and any [Agent Client Protocol](https://github.com/anthropics/agent-protocol)-compatible coding CLI (Kiro CLI, Claude Code, Codex, Gemini, Copilot CLI, etc.) over stdio JSON-RPC — delivering the next-generation development experience.

🪼 **Join our community!** Come say hi on Discord — we'd love to have you: **[🪼 OpenAB — Official](https://discord.gg/YNksK9M6)** 🎉

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│ openab       │──────────────►│  coding CLI  │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│  (acp mode)  │
└──────────────┘               └──────────────┘               └──────────────┘
```

## Demo

![openab demo](images/demo.png)

## Features

- **Pluggable agent backend** — swap between Kiro CLI, Claude Code, Codex, Gemini, Copilot CLI via config
- **@mention trigger** — mention the bot in an allowed channel to start a conversation
- **Thread-based multi-turn** — auto-creates threads; no @mention needed for follow-ups
- **Edit-streaming** — live-updates the Discord message every 1.5s as tokens arrive
- **Emoji status reactions** — 👀→🤔→🔥/👨‍💻/⚡→👍+random mood face
- **Session pool** — one CLI process per thread, auto-managed lifecycle
- **ACP protocol** — JSON-RPC over stdio with tool call, thinking, and permission auto-reply support
- **Kubernetes-ready** — Dockerfile + k8s manifests with PVC for auth persistence
- **Voice message STT** — auto-transcribes Discord voice messages via Groq, OpenAI, or local Whisper server ([docs/stt.md](docs/stt.md))

## Quick Start

### Prerequisites

Before running openab, enable these in the [Discord Developer Portal](https://discord.com/developers/applications):

1. **Bot → Privileged Gateway Intents**:
   - ✅ Message Content Intent
   - ✅ Server Members Intent
2. **OAuth2 → URL Generator → Bot Permissions**:
   - Send Messages, Embed Links, Attach Files
   - Read Message History, Add Reactions

See [docs/discord-bot-howto.md](docs/discord-bot-howto.md) for a detailed step-by-step guide.

### 1. Create a Discord Bot

Follow the [prerequisites](#prerequisites) above, then create your bot.

### 2. Install with Helm (Kiro CLI — default)

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update

helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'
```

### 3. Authenticate (first time only)

```bash
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow
kubectl rollout restart deployment/openab-kiro
```

### 4. Use

In your Discord channel:
```
@YourBot explain this code
```

The bot creates a thread. After that, just type in the thread — no @mention needed.

## Other Agents

| Agent | CLI | ACP Adapter | Guide |
|-------|-----|-------------|-------|
| Kiro (default) | `kiro-cli acp` | Native | [docs/kiro.md](docs/kiro.md) |
| Claude Code | `claude-agent-acp` | [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) | [docs/claude-code.md](docs/claude-code.md) |
| Codex | `codex-acp` | [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) | [docs/codex.md](docs/codex.md) |
| Gemini | `gemini --acp` | Native | [docs/gemini.md](docs/gemini.md) |
| Copilot CLI ⚠️ | `copilot --acp --stdio` | Native | [docs/copilot.md](docs/copilot.md) |

> 🔧 Running multiple agents? See [docs/multi-agent.md](docs/multi-agent.md)

## Local Development

```bash
cp config.toml.example config.toml
# Edit config.toml with your bot token and channel ID

export DISCORD_BOT_TOKEN="your-token"
cargo run
```

## Configuration Reference

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"   # supports env var expansion
allowed_channels = ["123456789"]      # channel ID allowlist
# allowed_users = ["987654321"]       # user ID allowlist (empty = all users)

[agent]
command = "kiro-cli"                  # CLI command
args = ["acp", "--trust-all-tools"]   # ACP mode args
working_dir = "/tmp"                  # agent working directory
env = {}                              # extra env vars passed to the agent

[pool]
max_sessions = 10                     # max concurrent sessions
session_ttl_hours = 24                # idle session TTL

[reactions]
enabled = true                        # enable emoji status reactions
remove_after_reply = false            # remove reactions after reply
```

<details>
<summary>Full reactions config</summary>

```toml
[reactions.emojis]
queued = "👀"
thinking = "🤔"
tool = "🔥"
coding = "👨‍💻"
web = "⚡"
done = "🆗"
error = "😱"

[reactions.timing]
debounce_ms = 700
stall_soft_ms = 10000
stall_hard_ms = 30000
done_hold_ms = 1500
error_hold_ms = 2500
```

</details>

## Kubernetes Deployment

The Docker image bundles both `openab` and `kiro-cli` in a single container.

```
┌─ Kubernetes Pod ──────────────────────────────────────┐
│  openab (PID 1)                                       │
│    └─ kiro-cli acp --trust-all-tools (child process)  │
│       ├─ stdin  ◄── JSON-RPC requests                 │
│       └─ stdout ──► JSON-RPC responses                │
│                                                       │
│  PVC (/data)                                          │
│    ├─ ~/.kiro/                  (settings, sessions)  │
│    └─ ~/.local/share/kiro-cli/  (OAuth tokens)        │
└───────────────────────────────────────────────────────┘
```

### Build & Push

```bash
docker build -t openab:latest .
docker tag openab:latest <your-registry>/openab:latest
docker push <your-registry>/openab:latest
```

### Deploy without Helm

```bash
kubectl create secret generic openab-secret \
  --from-literal=discord-bot-token="your-token"

kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/pvc.yaml
kubectl apply -f k8s/deployment.yaml
```

| Manifest | Purpose |
|----------|---------|
| `k8s/deployment.yaml` | Single-container pod with config + data volume mounts |
| `k8s/configmap.yaml` | `config.toml` mounted at `/etc/openab/` |
| `k8s/secret.yaml` | `DISCORD_BOT_TOKEN` injected as env var |
| `k8s/pvc.yaml` | Persistent storage for auth + settings |

## Project Structure

```
├── Dockerfile          # multi-stage: rust build + debian-slim runtime with kiro-cli
├── config.toml.example # example config with all agent backends
├── k8s/                # Kubernetes manifests
└── src/
    ├── main.rs         # entrypoint: tokio + serenity + cleanup + shutdown
    ├── config.rs       # TOML config + ${ENV_VAR} expansion
    ├── discord.rs      # Discord bot: mention, threads, edit-streaming
    ├── format.rs       # message splitting (2000 char limit)
    ├── reactions.rs    # status reaction controller (debounce, stall detection)
    └── acp/
        ├── protocol.rs # JSON-RPC types + ACP event classification
        ├── connection.rs # spawn CLI, stdio JSON-RPC communication
        └── pool.rs     # thread_id → AcpConnection map
```

## Inspired By

- [sample-acp-bridge](https://github.com/aws-samples/sample-acp-bridge) — ACP protocol + process pool architecture
- [OpenClaw](https://github.com/openclaw/openclaw) — StatusReactionController emoji pattern

## License

MIT

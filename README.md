# OpenAB — Open Agent Broker

A lightweight, secure, cloud-native ACP harness that bridges Discord and any [Agent Client Protocol](https://github.com/anthropics/agent-protocol)-compatible coding CLI (Kiro CLI, Claude Code, Codex, Gemini, etc.) over stdio JSON-RPC — delivering the next-generation development experience.

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

- **Pluggable agent backend** — swap between Kiro CLI, Claude Code, Codex, Gemini via config
- **@mention trigger** — mention the bot in an allowed channel to start a conversation
- **Thread-based multi-turn** — auto-creates threads; no @mention needed for follow-ups
- **Edit-streaming** — live-updates the Discord message every 1.5s as tokens arrive
- **Emoji status reactions** — 👀→🤔→🔥/👨‍💻/⚡→👍+random mood face
- **Session pool** — one CLI process per thread, auto-managed lifecycle
- **ACP protocol** — JSON-RPC over stdio with tool call, thinking, and permission auto-reply support
- **Kubernetes-ready** — Dockerfile + k8s manifests with PVC for auth persistence
- **Voice message STT** — auto-transcribes Discord voice messages via Groq, OpenAI, or local Whisper server ([docs/stt.md](docs/stt.md))

## Quick Start

### 1. Create a Discord Bot

See [docs/discord-bot-howto.md](docs/discord-bot-howto.md) for a detailed step-by-step guide.

In short:

1. Go to https://discord.com/developers/applications and create an application
2. Bot tab → enable **Message Content Intent**
3. OAuth2 → URL Generator → scope: `bot` → permissions: Send Messages, Send Messages in Threads, Create Public Threads, Read Message History, Add Reactions, Manage Messages
4. Invite the bot to your server using the generated URL

### 2. Configure

```bash
cp config.toml.example config.toml
```

Edit `config.toml`:
```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allowed_channels = ["YOUR_CHANNEL_ID"]
# allowed_users = ["YOUR_USER_ID"]  # optional: restrict who can use the bot

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/tmp"
```

### 3. Build & Run

```bash
export DISCORD_BOT_TOKEN="your-token"

# Development
cargo run

# Production
cargo build --release
./target/release/openab config.toml
```

If no config path is given, it defaults to `config.toml` in the current directory.

### 4. Use

In your Discord channel:
```
@AgentBroker explain this code
```

The bot creates a thread. After that, just type in the thread — no @mention needed.

## Pluggable Agent Backends

Supports Kiro CLI, Claude Code, Codex, Gemini, and any ACP-compatible CLI.

| Agent key | CLI | ACP Adapter | Auth |
|-----------|-----|-------------|------|
| `kiro` (default) | Kiro CLI | Native `kiro-cli acp` | `kiro-cli login --use-device-flow` |
| `codex` | Codex | [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) | `codex login --device-auth` |
| `claude` | Claude Code | [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) | `claude setup-token` |
| `gemini` | Gemini CLI | Native `gemini --acp` | Google OAuth or `GEMINI_API_KEY` |
| `copilot` | GitHub Copilot CLI | Native `copilot --acp` | `gh auth login` or `copilot auth` |

### Helm Install (recommended)

See the **[Helm chart docs](https://openabdev.github.io/openab)** for full installation instructions, values reference, and multi-agent examples.

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update

# Kiro CLI only (default)
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Claude Code only (disable default kiro)
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.claude.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:latest \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node

# Copilot only (native --acp mode)
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.copilot.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.copilot.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.copilot.image=ghcr.io/openabdev/openab-copilot:latest \
  --set agents.copilot.command=copilot \
  --set 'agents.copilot.args={--acp}' \
  --set agents.copilot.workingDir=/home/node

# Multi-agent (kiro + claude in one release)
helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$KIRO_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=KIRO_CHANNEL_ID' \
  --set agents.claude.discord.botToken="$CLAUDE_BOT_TOKEN" \
  --set-string 'agents.claude.discord.allowedChannels[0]=CLAUDE_CHANNEL_ID' \
  --set agents.claude.image=ghcr.io/openabdev/openab-claude:latest \
  --set agents.claude.command=claude-agent-acp \
  --set agents.claude.workingDir=/home/node
```

Each agent key in `agents` map creates its own Deployment, ConfigMap, Secret, and PVC. Set `agents.<name>.enabled: false` to skip creating resources for an agent.

### Manual config.toml

For non-Helm deployments, configure the `[agent]` block per CLI:

```toml
# Kiro CLI (default)
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

# Codex (requires codex-acp in PATH)
[agent]
command = "codex-acp"
args = []
working_dir = "/home/node"

# Claude Code (requires claude-agent-acp in PATH)
[agent]
command = "claude-agent-acp"
args = []
working_dir = "/home/node"

# Gemini
[agent]
command = "gemini"
args = ["--acp"]

# GitHub Copilot (native ACP — requires copilot CLI in PATH)
[agent]
command = "copilot"
args = ["--acp"]
working_dir = "/home/node"
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }
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

[reactions.emojis]
queued = "👀"
thinking = "🤔"
tool = "🔥"
coding = "👨‍💻"
web = "⚡"
done = "🆗"
error = "😱"

[reactions.timing]
debounce_ms = 700                     # intermediate state debounce
stall_soft_ms = 10000                 # 10s idle → 🥱
stall_hard_ms = 30000                 # 30s idle → 😨
done_hold_ms = 1500                   # keep done emoji for 1.5s
error_hold_ms = 2500                  # keep error emoji for 2.5s
```

## Kubernetes Deployment

The Docker image bundles both `openab` and `kiro-cli` in a single container (openab spawns kiro-cli as a child process).

### Pod Architecture

```
┌─ Kubernetes Pod ─────────────────────────────────────────────────┐
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐     │
│  │  openab (main process, PID 1)                           │     │
│  │                                                         │     │
│  │  ┌──────────────┐   ┌──────────────┐   ┌───────────┐    │     │
│  │  │ Discord      │   │ Session Pool │   │ Reaction  │    │     │
│  │  │ Gateway WS   │   │ (per thread) │   │ Controller│    │     │
│  │  └──────┬───────┘   └──────┬───────┘   └───────────┘    │     │
│  │         │                  │                            │     │
│  └─────────┼──────────────────┼────────────────────────────┘     │
│            │                  │                                  │
│            │ @mention /       │ spawn + stdio                    │
│            │ thread msg       │ JSON-RPC (ACP)                   │
│            │                  │                                  │
│            ▼                  ▼                                  │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │  kiro-cli acp --trust-all-tools  (child process)         │    │
│  │                                                          │    │
│  │  stdin  ◄── JSON-RPC requests  (session/new, prompt)     │    │
│  │  stdout ──► JSON-RPC responses (text, tool_call, done)   │    │
│  │  stderr ──► (ignored)                                    │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─ PVC Mount (/data) ──────────────────────────────────────┐    │
│  │  ~/.kiro/              ← settings, skills, sessions      │    │
│  │  ~/.local/share/kiro-cli/ ← OAuth tokens (data.sqlite3)  │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
         │
         │ WebSocket (wss://gateway.discord.gg)
         ▼
┌──────────────────┐         ┌──────────────┐
│  Discord API     │ ◄─────► │  Discord     │
│  Gateway         │         │  Users       │
└──────────────────┘         └──────────────┘
```

- **Single container** — openab is PID 1, spawns kiro-cli as a child process
- **stdio JSON-RPC** — ACP communication over stdin/stdout, no network ports needed
- **Session pool** — one kiro-cli process per Discord thread, up to `max_sessions`
- **PVC** — persists OAuth tokens and settings across pod restarts

### Install with Your Coding CLI

See the **[Helm chart docs](https://openabdev.github.io/openab)** for per-agent install commands (Kiro CLI, Claude Code, Codex, Gemini) and values reference.

### Build & Push

```bash
docker build -t openab:latest .
docker tag openab:latest <your-registry>/openab:latest
docker push <your-registry>/openab:latest
```

### Deploy

```bash
# Create the secret with your bot token
kubectl create secret generic openab-secret \
  --from-literal=discord-bot-token="your-token"

# Edit k8s/configmap.yaml with your channel IDs
kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/pvc.yaml
kubectl apply -f k8s/deployment.yaml
```

### Authenticate kiro-cli (first time only)

kiro-cli requires a one-time OAuth login. The PVC persists the tokens across pod restarts.

```bash
kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow
```

Follow the device code flow in your browser, then restart the pod:

```bash
kubectl rollout restart deployment/openab-kiro
```

### Manifests

| File | Purpose |
|------|---------|
| `k8s/deployment.yaml` | Single-container pod with config + data volume mounts |
| `k8s/configmap.yaml` | `config.toml` mounted at `/etc/openab/` |
| `k8s/secret.yaml` | `DISCORD_BOT_TOKEN` injected as env var |
| `k8s/pvc.yaml` | Persistent storage for auth + settings |

The PVC persists two paths via `subPath`:
- `~/.kiro` — settings, skills, sessions
- `~/.local/share/kiro-cli` — OAuth tokens (`data.sqlite3` → `auth_kv` table), conversation history

## Project Structure

```
├── Dockerfile          # multi-stage: rust build + debian-slim runtime with kiro-cli
├── config.toml.example # example config with all agent backends
├── k8s/                # Kubernetes manifests
│   ├── deployment.yaml
│   ├── configmap.yaml
│   ├── secret.yaml
│   └── pvc.yaml
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

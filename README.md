# OpenAB — Open Agent Broker

[![Stars](https://img.shields.io/github/stars/openabdev/openab?style=flat-square)](https://github.com/openabdev/openab) [![GitHub Release](https://img.shields.io/github/v/release/openabdev/openab?style=flat-square&logo=github)](https://github.com/openabdev/openab/releases/latest) ![License](https://img.shields.io/badge/license-MIT-A374ED?style=flat-square)

![OpenAB banner](images/banner.jpg)

A lightweight, secure, cloud-native ACP harness that bridges **Discord, Slack**, and any [Agent Client Protocol](https://github.com/anthropics/agent-protocol)-compatible coding CLI (Kiro CLI, Claude Code, Codex, Gemini, OpenCode, Copilot CLI, etc.) over stdio JSON-RPC — delivering the next-generation development experience. **Telegram, LINE, Feishu/Lark, Google Chat**, and other webhook-based platforms are supported via the standalone [Custom Gateway](gateway/).

🪼 **Join our community!** Come say hi on Discord — we'd love to have you: **[🪼 OpenAB — Official](https://discord.gg/DmbhfDZjQS)** 🎉

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│              │──────────────►│  coding CLI  │
│   User       │               │    openab    │◄── JSON-RPC ──│  (acp mode)  │
├──────────────┤  Socket Mode  │    (Rust)    │               └──────────────┘
│   Slack      │◄─────────────►│              │
│   User       │               └──────┬───────┘
├──────────────┤                      │ WebSocket (outbound)
│   Telegram   │◄──webhook──┐         │
│   User       │            │         │
├──────────────┤            ▼         ▼
│   LINE       │◄──webhook──┌──────────────────┐
│   User       │            │  Custom Gateway  │
├──────────────┤            │  (standalone)    │
│  Feishu/Lark │◄───WS──────│                  │
│   User       │            │                  │
├──────────────┤            │                  │
│ Google Chat  │◄──webhook──│                  │
│   User       │            └──────────────────┘
└──────────────┘
```

## Demo

![openab demo](images/demo.png)

## Features

- **Multi-platform** — supports Discord and Slack, run one or both simultaneously
- **Custom Gateway** — extend to Telegram, LINE, Feishu/Lark, Google Chat, MS Teams via standalone [gateway](gateway/)
- **Pluggable agent backend** — swap between Kiro CLI, Claude Code, Codex, Gemini, OpenCode, Copilot CLI via config
- **@mention trigger** — mention the bot in an allowed channel to start a conversation
- **Thread-based multi-turn** — auto-creates threads; no @mention needed for follow-ups
- **Multi-agent collaboration** — bot-to-bot messaging for coordinated workflows ([docs/multi-agent.md](docs/multi-agent.md))
- **Edit-streaming** — live-updates the Discord message every 1.5s as tokens arrive
- **Emoji status reactions** — 👀→🤔→🔥/👨‍💻/⚡→👍+random mood face
- **Image & file support** — send images and files through chat ([docs/sendimages.md](docs/sendimages.md), [docs/sendfiles.md](docs/sendfiles.md))
- **Scheduled messages** — config-driven cron jobs for automated agent prompts ([docs/cronjob.md](docs/cronjob.md))
- **Slash commands** — built-in slash command support ([docs/slash-commands.md](docs/slash-commands.md))
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

See [docs/discord.md](docs/discord.md) for a detailed step-by-step guide.

### 1. Create a Bot

<details>
<summary><strong>Discord</strong></summary>

See [docs/discord.md](docs/discord.md) for a detailed step-by-step guide.

</details>

<details>
<summary><strong>Slack</strong></summary>

See [docs/slack-bot-howto.md](docs/slack-bot-howto.md) for a detailed step-by-step guide.

</details>

<details>
<summary><strong>Telegram</strong> (via Custom Gateway)</summary>

See [docs/telegram.md](docs/telegram.md) for the full setup guide. Requires the standalone [Custom Gateway](gateway/) service.

</details>

<details>
<summary><strong>LINE</strong> (via Custom Gateway)</summary>

See [docs/line.md](docs/line.md) for the full setup guide. Requires the standalone [Custom Gateway](gateway/) service.

</details>

<details>
<summary><strong>Feishu/Lark</strong> (via Custom Gateway)</summary>

See [docs/feishu.md](docs/feishu.md) for the full setup guide. Requires the standalone [Custom Gateway](gateway/) service. Supports WebSocket long-connection (default, no public URL needed) and HTTP webhook fallback.

</details>

<details>
<summary><strong>Google Chat</strong> (via Custom Gateway)</summary>

See [docs/google-chat.md](docs/google-chat.md) for the full setup guide. Requires the standalone [Custom Gateway](gateway/) service.

</details>

### 2. Install with Helm (Kiro CLI — default)

```bash
helm repo add openab https://openabdev.github.io/openab
helm repo update

helm install openab openab/openab \
  --set agents.kiro.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.kiro.discord.allowedChannels[0]=YOUR_CHANNEL_ID'

# Slack
helm install openab openab/openab \
  --set agents.kiro.slack.enabled=true \
  --set agents.kiro.slack.botToken="$SLACK_BOT_TOKEN" \
  --set agents.kiro.slack.appToken="$SLACK_APP_TOKEN" \
  --set-string 'agents.kiro.slack.allowedChannels[0]=C0123456789'
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

**Slack:** `@YourBot explain this code` in a channel — same thread-based workflow as Discord.

## Other Agents

| Agent | CLI | ACP Adapter | Guide |
|-------|-----|-------------|-------|
| Kiro (default) | `kiro-cli acp` | Native | [docs/kiro.md](docs/kiro.md) |
| Claude Code | `claude-agent-acp` | [@agentclientprotocol/claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp) | [docs/claude-code.md](docs/claude-code.md) |
| Codex | `codex-acp` | [@zed-industries/codex-acp](https://github.com/zed-industries/codex-acp) | [docs/codex.md](docs/codex.md) |
| Gemini | `gemini --acp` | Native | [docs/gemini.md](docs/gemini.md) |
| OpenCode | `opencode acp` | Native | [docs/opencode.md](docs/opencode.md) |
| Copilot CLI ⚠️ | `copilot --acp --stdio` | Native | [docs/copilot.md](docs/copilot.md) |
| Cursor | `cursor-agent acp` | Native | [docs/cursor.md](docs/cursor.md) |

> 🔧 Running multiple agents? See [docs/multi-agent.md](docs/multi-agent.md)

## Configuration Reference

> 📖 Full reference with all options, defaults, and Helm mapping: [docs/config-reference.md](docs/config-reference.md)

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"   # supports env var expansion
allowed_channels = ["123456789"]      # channel ID allowlist
# allowed_users = ["987654321"]       # user ID allowlist (empty = all users)

[slack]
bot_token = "${SLACK_BOT_TOKEN}"     # Bot User OAuth Token (xoxb-...)
app_token = "${SLACK_APP_TOKEN}"     # App-Level Token (xapp-...) for Socket Mode
allowed_channels = ["C0123456789"]   # channel ID allowlist (empty = allow all)
# allowed_users = ["U0123456789"]    # user ID allowlist (empty = allow all)

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

## Inspired By

- [sample-acp-bridge](https://github.com/aws-samples/sample-acp-bridge) — ACP protocol + process pool architecture
- [OpenClaw](https://github.com/openclaw/openclaw) — StatusReactionController emoji pattern

## License

MIT

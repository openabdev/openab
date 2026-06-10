# OpenShell

Run OAB inside an [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell) sandbox for isolated, policy-enforced execution.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Host (Linux with Docker)                                           │
│                                                                     │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  OpenShell Gateway (systemd user service :17670)              │  │
│  │  • manages sandbox lifecycle                                  │  │
│  │  • enforces network policy (default-deny egress)              │  │
│  │  • injects provider credentials into sandbox env              │  │
│  └──────────────────────────┬────────────────────────────────────┘  │
│                             │ creates & policies                     │
│                             ▼                                        │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │  Docker Container (sandbox: "oab")                            │  │
│  │                                                               │  │
│  │  /home/sandbox/                                               │  │
│  │  └── config.toml         ← bot token + agent config           │  │
│  │                                                               │  │
│  │  openab run ──stdio JSON-RPC──► openab-agent                  │  │
│  │       │                              │                        │  │
│  │       │ Discord WS                   │ ChatGPT API            │  │
│  └───────┼──────────────────────────────┼────────────────────────┘  │
│           │                              │                           │
│  ┌────────┼──────────────────────────────┼────────────────────┐     │
│  │ Network Policy (egress allowlist)     │                    │     │
│  │  ✓ discord.com:443                    │                    │     │
│  │  ✓ gateway.discord.gg:443            │                    │     │
│  │  ✓ cdn.discordapp.com:443            │                    │     │
│  │  ✓ chatgpt.com:443  ◄────────────────┘                    │     │
│  │  ✓ auth0.openai.com:443                                   │     │
│  │  ✗ everything else DENIED                                  │     │
│  └────────┼───────────────────────────────────────────────────┘     │
└───────────┼─────────────────────────────────────────────────────────┘
            │
            ▼
┌──────────────────┐         ┌──────────────────┐
│  Discord API     │         │  ChatGPT API     │
│  (bot gateway)   │         │  (chatgpt.com)   │
└──────────────────┘         └──────────────────┘
```

## Prerequisites

- Docker running on the host (user must be in the `docker` group)
- [OpenShell CLI](https://github.com/NVIDIA/OpenShell#install) installed

```bash
curl -LsSf https://raw.githubusercontent.com/NVIDIA/OpenShell/main/install.sh | sh
```

> **Note:** If the gateway fails with "failed to query Docker daemon version", add your user to the `docker` group and re-login:
> ```bash
> sudo usermod -aG docker $USER
> # Log out and back in (or: loginctl terminate-user $USER)
> ```

## Quick Start

### 1. Create credential provider

```bash
openshell provider create --name discord --type generic \
  --credential "DISCORD_BOT_TOKEN=your-token-here"
```

### 2. Create sandbox

Using the pre-built image:

```bash
openshell sandbox create --name oab \
  --from ghcr.io/openabdev/openab-native-sandbox:latest \
  --provider discord \
  -- bash
```

Or build locally from `openshell/Dockerfile`:

```bash
docker build -t oab-sandbox openshell/
openshell sandbox create --name oab \
  --from oab-sandbox:latest \
  --provider discord \
  -- bash
```

### 3. Set network policy (from host)

```bash
openshell policy update oab \
  --add-endpoint "discord.com:443:read-write:rest:enforce" \
  --add-endpoint "gateway.discord.gg:443:read-write:websocket:enforce" \
  --add-endpoint "cdn.discordapp.com:443:read-write:rest:enforce" \
  --add-endpoint "chatgpt.com:443:read-write:rest:enforce" \
  --add-endpoint "auth0.openai.com:443:read-write:rest:enforce"
```

### 4. Authenticate (inside sandbox)

```bash
sandbox$ openab-agent auth codex-oauth --no-browser
```

Open the printed URL in your browser, approve, then paste the `localhost:1455/auth/callback?...` URL back.

### 5. Create config and run

```bash
sandbox$ cat > config.toml <<'EOF'
[discord]
bot_token = "your-bot-token"
allow_all_channels = true

[agent]
command = "openab-agent"
working_dir = "/home/sandbox"
env = { OPENAB_AGENT_OPENAI_MODEL = "gpt-5.4-mini" }

[pool]
max_sessions = 3
session_ttl_hours = 1

[reactions]
enabled = true
EOF

sandbox$ openab run --config config.toml
```

## Network Policy

OpenShell sandboxes have **default-deny egress**. Required endpoints by backend:

| Backend | Endpoints |
|---------|-----------|
| All | `discord.com:443`, `gateway.discord.gg:443`, `cdn.discordapp.com:443` |
| Native Agent (codex) | `chatgpt.com:443`, `auth0.openai.com:443` |
| Native Agent (anthropic) | `api.anthropic.com:443` |
| GitHub access | `api.github.com:443`, `github.com:443` |

## Reconnecting

```bash
openshell sandbox connect oab
```

## Cleanup

```bash
openshell sandbox delete oab
openshell provider delete discord
```

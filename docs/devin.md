# Devin CLI — Agent Backend Guide

How to run OpenAB with [Devin CLI](https://docs.devin.ai/cli) as the agent backend.

## Prerequisites

- A [Devin](https://devin.ai/) subscription (Enterprise or individual plan)
- Devin CLI with native ACP support (`devin acp`)

## Architecture

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│ openab       │──────────────►│ devin acp    │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│ (Devin CLI)  │
└──────────────┘               └──────────────┘               └──────────────┘
```

OpenAB spawns `devin acp` as a child process and communicates via stdio JSON-RPC. No intermediate adapter needed — Devin CLI natively implements the Agent Client Protocol.

## Configuration

```toml
[agent]
command = "devin"
args = ["acp"]
working_dir = "/home/agent"
```

## Docker

Build with the unified Dockerfile:

```bash
docker build --target devin -f Dockerfile.unified -t openab-devin .
```

Or via docker buildx bake:

```bash
docker buildx bake devin
```

The Dockerfile installs Devin CLI via the official install script (`https://cli.devin.ai/install.sh`).

## Authentication

Devin CLI requires authentication via a Devin account. In a headless container:

```bash
# 1. Exec into the running pod/container
kubectl exec -it deployment/openab-devin -- bash

# 2. Authenticate via manual token flow (headless-friendly)
devin auth login --force-manual-token-flow

# 3. Follow the instructions to paste your token

# 4. Restart the pod (credentials persist via PVC)
kubectl rollout restart deployment/openab-devin
```

Credentials are stored under `~/.config/devin/` and persist across pod restarts via PVC.

### Alternative: WINDSURF_API_KEY

For legacy Windsurf credentials, set the `WINDSURF_API_KEY` environment variable:

```toml
[agent]
command = "devin"
args = ["acp"]
working_dir = "/home/agent"
env = { WINDSURF_API_KEY = "${WINDSURF_API_KEY}" }
```

## Helm Install

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.devin.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.devin.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.devin.command=devin \
  --set 'agents.devin.args={acp}' \
  --set agents.devin.persistence.enabled=true \
  --set agents.devin.workingDir=/home/agent \
  --set image.tag=beta
```

### Image Tag

Use `--set image.tag=<version>` to set the image version globally.
The chart auto-appends `-<agent>` to produce the final tag (see [image-tags.md](image-tags.md) for full details).

| Tag | Resolves to | Description |
|-----|-------------|-------------|
| `beta` | `beta-devin` | Floating beta channel (latest pre-release) |
| `stable` | `stable-devin` | Floating stable channel |

## Features

Devin CLI provides:

- **Native ACP**: `devin acp` speaks JSON-RPC over stdio directly
- **AGENTS.md support**: Reads `AGENTS.md` at project root automatically
- **MCP servers**: Full MCP support (stdio + HTTP transports)
- **Subagents**: Can spawn foreground/background subagents for parallel work
- **Session persistence**: Conversation history saved and resumable
- **Models**: SWE-1.6 series with adaptive routing

## AGENTS.md Compatibility

Devin CLI reads `AGENTS.md` from the project root — the same file OpenAB already uses. It also reads `CLAUDE.md` (for Claude Code compatibility) and rules from `.cursor/rules/` and `.windsurf/rules/`.

## Known Limitations

- Requires a paid Devin subscription; no free tier for CLI access
- `devin auth login` requires interactive terminal for browser flow; use `--force-manual-token-flow` in headless environments
- Enterprise features (team settings, controls) require Devin Enterprise plan
- Devin CLI is the rebranded Windsurf CLI (Codeium); legacy credentials still work via `WINDSURF_API_KEY`

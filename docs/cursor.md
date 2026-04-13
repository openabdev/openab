# Cursor Agent CLI — Agent Backend Guide

How to run OpenAB with [Cursor Agent CLI](https://www.cursor.com/) as the agent backend.

## Prerequisites

- A paid [Cursor](https://www.cursor.com/pricing) subscription (**Pro or Business** — Free tier does not include Agent CLI access)
- Cursor Agent CLI with native ACP support

## Architecture

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────────────┐
│   Discord    │◄─────────────►│ openab       │──────────────►│ cursor-agent acp      │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│ (Cursor Agent CLI)    │
└──────────────┘               └──────────────┘               └──────────────────────┘
```

OpenAB spawns `cursor-agent acp` as a child process and communicates via stdio JSON-RPC. No intermediate layers.

## Configuration

```toml
[agent]
command = "cursor-agent"
args = ["acp"]
working_dir = "/home/agent"
# Auth via: kubectl exec -it <pod> -- cursor-agent login
```

## Docker

Build with the Cursor-specific Dockerfile:

```bash
docker build -f Dockerfile.cursor -t openab-cursor .
```

The Dockerfile installs a pinned version of Cursor Agent CLI via direct download from `downloads.cursor.com`. The version is controlled by the `CURSOR_VERSION` build arg.

## Authentication

Cursor Agent CLI uses its own login flow. In a headless container:

```bash
# 1. Exec into the running pod/container
kubectl exec -it deployment/openab-cursor -- bash

# 2. Authenticate via device flow
cursor-agent login

# 3. Follow the device code flow in your browser

# 4. Restart the pod (token is persisted via PVC)
kubectl rollout restart deployment/openab-cursor
```

The auth token is stored under `~/.cursor/` and persisted across pod restarts via PVC.

## Helm Install

> **Note**: The `ghcr.io/openabdev/openab-cursor` image is not published yet. You must build it locally first with `docker build -f Dockerfile.cursor -t openab-cursor .` and push to your own registry, or use a local image.

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.cursor.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.cursor.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.cursor.image=ghcr.io/openabdev/openab-cursor:latest \
  --set agents.cursor.command=cursor-agent \
  --set 'agents.cursor.args={acp}' \
  --set agents.cursor.persistence.enabled=true \
  --set agents.cursor.workingDir=/home/node
```

## Known Limitations

- Cursor Agent CLI is a separate distribution from Cursor Desktop — they are not the same binary
- No official apt/yum package; the Dockerfile downloads a pinned tarball directly
- `cursor-agent login` requires an interactive terminal for the device flow
- Auth token persistence requires a PVC mount at the user home directory

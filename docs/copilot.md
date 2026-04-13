# GitHub Copilot CLI — Agent Backend Guide

How to run OpenAB with [GitHub Copilot CLI](https://github.com/github/copilot-cli) as the agent backend.

## Prerequisites

- A paid [GitHub Copilot](https://github.com/features/copilot/plans) subscription (**Pro, Pro+, Business, or Enterprise** — Free tier does not include CLI/ACP access)
- Copilot CLI ACP support is in [public preview](https://github.blog/changelog/2026-01-28-acp-support-in-copilot-cli-is-now-in-public-preview/) since Jan 28, 2026

## Architecture

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────────────┐
│   Discord    │◄─────────────►│ openab       │──────────────►│ copilot --acp --stdio │
│   User       │               │   (Rust)     │◄── JSON-RPC ──│ (Copilot CLI)         │
└──────────────┘               └──────────────┘               └──────────────────────┘
```

OpenAB spawns `copilot --acp --stdio` as a child process and communicates via stdio JSON-RPC. No intermediate layers.

## Configuration

```toml
[agent]
command = "copilot"
args = ["--acp", "--stdio"]
working_dir = "/home/agent"
# Auth via: kubectl exec -it <pod> -- gh auth login -p https -w
```

## Docker

Build with the Copilot-specific Dockerfile:

```bash
docker build -f Dockerfile.copilot -t openab-copilot .
```

## Authentication

Copilot CLI uses GitHub OAuth (same as `gh` CLI). In a headless container, use device flow:

```bash
# 1. Exec into the running pod/container
kubectl exec -it deployment/openab-copilot -- bash

# 2. Authenticate via device flow
gh auth login --hostname github.com --git-protocol https -p https -w

# 3. Follow the device code flow in your browser

# 4. Verify
gh auth status

# 5. Restart the pod (token is persisted via PVC)
kubectl rollout restart deployment/openab-copilot
```

The OAuth token is stored under `~/.config/gh/` and persisted across pod restarts via PVC.

> **Note**: See [docs/gh-auth-device-flow.md](gh-auth-device-flow.md) for details on device flow in headless environments.

## Helm Install

> **Note**: The `ghcr.io/openabdev/openab-copilot` image is not published yet. You must build it locally first with `docker build -f Dockerfile.copilot -t openab-copilot .` and push to your own registry, or use a local image.

```bash
helm install openab openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.copilot.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.copilot.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.copilot.image=ghcr.io/openabdev/openab-copilot:latest \
  --set agents.copilot.command=copilot \
  --set 'agents.copilot.args={--acp,--stdio}' \
  --set agents.copilot.persistence.enabled=true \
  --set agents.copilot.workingDir=/home/node
```

## Model Selection

Copilot CLI defaults to Claude Sonnet 4.6. Other available models include:

- Claude Opus 4.6, Claude Haiku 4.5 (Anthropic)
- GPT-5.3-Codex (OpenAI)
- Gemini 3 Pro (Google)

Model selection is controlled by Copilot CLI itself (via `/model` in interactive mode). In ACP mode, the default model is used.

## Known Limitations

- ⚠️ ACP support is in **public preview** — behavior may change
- ⚠️ Headless auth with `GITHUB_TOKEN` env var has not been fully validated; device flow via `gh auth login` is the recommended path
- Copilot CLI requires an active Copilot subscription per user/org
- For Copilot Business/Enterprise, an admin must enable Copilot CLI from the Policies page

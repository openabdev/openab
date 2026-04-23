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
working_dir = "/home/node"
```

## Docker

Build with the Copilot-specific Dockerfile:

```bash
docker build -f Dockerfile.copilot -t openab-copilot .
```

## Authentication

Copilot CLI has two independent auth layers that can use **different** GitHub accounts:

1. **Copilot subscription auth** — authenticates your Copilot subscription (model access)
2. **`gh` CLI auth** — authenticates git operations (clone, push, PR creation)

This separation lets you use a subscription owner's token for Copilot while scoping git operations to a different GitHub user (e.g. a bot account).

### Step 1: Copilot Subscription (fine-grained PAT)

Generate a [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new) from the GitHub account that owns the Copilot subscription:

- Token name: e.g. `openab-copilot`
- Expiration: as needed
- **Account permissions → Copilot Requests: Read-only** (this is the only permission required)

Inject it as an env var in your Helm chart (add the last line):

```bash
helm install openab-copilot openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.copilot.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.copilot.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.copilot.discord.enabled=true \
  --set agents.copilot.image=ghcr.io/openabdev/openab-copilot \
  --set agents.copilot.command=copilot \
  --set 'agents.copilot.args={--acp,--stdio}' \
  --set agents.copilot.persistence.enabled=true \
  --set agents.copilot.workingDir=/home/node \
  --set 'agents.copilot.env.COPILOT_GITHUB_TOKEN=github_pat_YOUR_TOKEN_HERE'  # optional
```

> **Note**: `COPILOT_GITHUB_TOKEN` is only required if you want to authenticate the Copilot subscription via a fine-grained PAT without running `copilot login`, or if you plan to use `gh auth login` with a different user for git operations. If you only have one GitHub account, you can skip this and use `copilot login` instead (see below).

### Step 2: `gh` CLI Auth (scoped user)

After deployment, authenticate `gh` as a separate user for git operations:

```bash
kubectl exec -it deployment/openab-copilot-copilot -- gh auth login -p https -w
```

Follow the device flow in your browser, authorizing with the desired GitHub account (e.g. a bot user like `thepagent`).

Verify:

```bash
kubectl exec deployment/openab-copilot-copilot -- gh auth status
```

The `gh` token is stored under `~/.config/gh/` on the PVC and persists across pod restarts.

### Summary

```
Scenario 1: Same user for both (simple)
┌─────────────────────────────────────────────────────────┐
│  copilot login (as @alice)                              │
│    ├─ Copilot subscription ── @alice's plan ✅          │
│    └─ gh operations ───────── @alice ✅                 │
│                                                         │
│  No env var needed. One login covers everything.        │
└─────────────────────────────────────────────────────────┘

Scenario 2: Different users (split auth)
┌─────────────────────────────────────────────────────────┐
│  COPILOT_GITHUB_TOKEN=github_pat_... (from @alice)      │
│    └─ Copilot subscription ── @alice's plan ✅          │
│                                                         │
│  gh auth login (as @bot-user)                           │
│    └─ gh operations ───────── @bot-user ✅              │
│                                                         │
│  Use when subscription owner ≠ git operations user.     │
│  e.g. @alice owns Copilot Pro, @bot-user pushes code.   │
└─────────────────────────────────────────────────────────┘
```

> **Recommendation**: If your Copilot subscription is on a privileged human account (e.g. org admin), we strongly recommend Scenario 2 — use a fine-grained PAT for the subscription and a scoped bot user for git operations. This limits the blast radius of the agent's git access.

| Auth Layer | Purpose | Account | Method |
|---|---|---|---|
| `COPILOT_GITHUB_TOKEN` | Copilot subscription (models) | Subscription owner | Fine-grained PAT env var |
| `gh auth` | Git operations (clone, push) | Bot / scoped user | Device flow (`gh auth login`) |

> **Note**: Classic personal access tokens (`ghp_`) are **not supported** for Copilot. Use a fine-grained PAT (`github_pat_`) with the "Copilot Requests" permission.

## Helm Install

```bash
helm install openab-copilot openab/openab \
  --set agents.kiro.enabled=false \
  --set agents.copilot.discord.enabled=true \
  --set agents.copilot.discord.botToken="$DISCORD_BOT_TOKEN" \
  --set-string 'agents.copilot.discord.allowedChannels[0]=YOUR_CHANNEL_ID' \
  --set agents.copilot.image=ghcr.io/openabdev/openab-copilot \
  --set agents.copilot.command=copilot \
  --set 'agents.copilot.args={--acp,--stdio}' \
  --set agents.copilot.persistence.enabled=true \
  --set agents.copilot.workingDir=/home/node \
  --set 'agents.copilot.env.COPILOT_GITHUB_TOKEN=github_pat_YOUR_TOKEN_HERE'  # optional, see Authentication
```

## Model Selection

Copilot CLI defaults to Claude Sonnet 4.6. Other available models include:

- Claude Opus 4.6, Claude Haiku 4.5 (Anthropic)
- GPT-5.3-Codex (OpenAI)
- Gemini 3 Pro (Google)

Model selection is controlled by Copilot CLI itself (via `/model` in interactive mode). In ACP mode, the default model is used.

## Known Limitations

- ⚠️ ACP support is in **public preview** — behavior may change
- Classic personal access tokens (`ghp_`) are not supported — use fine-grained PATs (`github_pat_`)
- Copilot CLI requires an active Copilot subscription per user/org
- For Copilot Business/Enterprise, an admin must enable Copilot CLI from the Policies page

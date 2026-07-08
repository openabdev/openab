# Devin CLI — Agent Backend Guide

How to run OpenAB with [Devin CLI](https://docs.devin.ai/cli) as the agent backend.

## Prerequisites

- A [Devin](https://devin.ai/) subscription (Enterprise or individual plan) from Cognition AI
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

[pool]
max_sessions = 3
session_ttl_hours = 1
default_config_options = { mode = "bypass", model = "swe-1-6" }
```

> **Note:** `devin acp` does not honor `~/.config/devin/config.json` settings for
> permission mode or model, nor does it read `DEVIN_PERMISSION_MODE` or `DEVIN_MODEL`
> env vars. The **only** way to set defaults at session start in ACP mode is via
> `[pool] default_config_options`, which sends `session/set_config_option` after each
> session creation. Without `mode = "bypass"`, Devin defaults to `accept-edits` mode
> which prompts for `exec` tool calls, causing the agent to get stuck in headless
> environments.

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

Credentials are stored under `~/.local/share/devin/` and persist across pod restarts via PVC.

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

## Model Selection

Devin CLI uses Adaptive routing by default. To specify a model, use `default_config_options`:

```toml
[pool]
default_config_options = { model = "swe-1-6" }
```

Available model values include: `adaptive`, `swe-1-6`, `swe-1-6-fast`, `claude-opus-4-8-medium`,
`glm-5-2`, `gpt-5-5-medium`, `kimi-k2-7`, and many more. The full list is reported by the
agent at session creation and visible via the Discord `/model` slash command.

> **Note:** The `--model` flag and `DEVIN_MODEL` env var are **ignored** in ACP mode.
> Use `default_config_options` instead.

## AGENTS.md Compatibility

Devin CLI reads `AGENTS.md` from the project root — the same file OpenAB already uses. It also reads `CLAUDE.md` (for Claude Code compatibility) and rules from `.cursor/rules/` and `.windsurf/rules/`.

## Recommended Permission Config (Headless)

In headless/container deployments, Devin CLI defaults to `accept-edits` mode in ACP
which prompts for `exec` tool calls. Since `devin acp` does not honor
`~/.config/devin/config.json`, env vars, or CLI flags for mode/model, the only
mechanism is OAB's `default_config_options`:

```toml
[pool]
default_config_options = { mode = "bypass", model = "swe-1-6" }
```

This sends `session/set_config_option` after each session creation, switching the
agent to bypass mode (auto-approve all tool calls) and selecting the model.

**What doesn't work in ACP mode:**

| Method | Result |
|--------|--------|
| `DEVIN_PERMISSION_MODE=dangerous` env var | Ignored in ACP mode |
| `DEVIN_MODEL=swe-1.6` env var | Ignored in ACP mode |
| `--permission-mode dangerous` CLI flag | Not supported by `devin acp` |
| `--model opus` CLI flag | Not supported by `devin acp` |
| `~/.config/devin/config.json` `permission_mode` | Not honored in ACP mode |
| `~/.config/devin/config.json` `agent.model` | Not honored in ACP mode |

### Full OAB Config Example

```toml
[agent]
command = "devin"
args = ["acp"]
working_dir = "/home/agent"
env = { GHPOOL_URL = "http://ghpool.openab.local:8080", PATH = "/home/agent/bin:/usr/local/bin:/usr/bin:/bin" }

[pool]
max_sessions = 3
session_ttl_hours = 1
default_config_options = { mode = "bypass", model = "swe-1-6" }
```

## Known Limitations

- Requires a paid Devin subscription (Cognition AI); no free tier for CLI access
- `devin auth login` requires interactive terminal for browser flow; use `--force-manual-token-flow` in headless environments
- Enterprise features (team settings, controls) require Devin Enterprise plan
- Config file must be mounted at `/etc/openab/config.toml` at runtime (not baked into image)

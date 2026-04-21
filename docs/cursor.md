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
  --set agents.cursor.workingDir=/home/agent
```

## Model Selection

List available models:

```bash
cursor-agent --list-models
# or
cursor-agent models
```

To specify a model, pass `--model` as an arg:

```toml
[agent]
command = "cursor-agent"
args = ["acp", "--model", "auto"]
```

In ACP mode, `--model` can be appended after `acp`. If omitted, the account default is used.

To verify which model is active, ask the agent "who are you" — the underlying model will typically self-identify (e.g. "I am Gemini, a large language model built by Google.").

## MCP Usage (ACP mode caveats)

Cursor Agent CLI supports MCP servers configured via `.cursor/mcp.json` in the active workspace directory. **Which directory counts as the workspace is determined by the `--workspace` flag** — if omitted, cursor-agent auto-detects from `cwd`, which is usually `/home/agent` in OpenAB containers via the Dockerfile `WORKDIR` directive but can drift in interactive or local runs. For reproducible MCP loading, pass `--workspace` explicitly:

```toml
[agent]
command = "cursor-agent"
args = ["acp", "--model", "auto", "--workspace", "/home/agent"]
```

This anchors:
- **MCP config lookup**: `/home/agent/.cursor/mcp.json`
- **Approval file path**: `/home/agent/.cursor/projects/home-agent/mcp-approvals.json` (slug = URL-safe workspace path)

Without `--workspace`, a different cwd would produce a different slug and cursor-agent would not find previously saved approvals.

### Example MCP config

```json
{
  "mcpServers": {
    "playwright": {
      "command": "/usr/bin/npx",
      "args": ["-y", "@playwright/mcp@latest"]
    }
  }
}
```

### Approval quirk in ACP mode

Cursor's `--approve-mcps` flag **does not apply in ACP mode** — it only affects the interactive CLI. In ACP mode, MCP servers are gated by an approval file. Two options:

1. **Pre-create the approvals file** at `<workspace>/.cursor/projects/<slug>/mcp-approvals.json`:
   ```json
   ["<server-name>-<sha256_hash>"]
   ```
   Hash is derived from workspace path + server config.

2. **Approve once interactively**, then let Cursor persist the approval:
   ```bash
   kubectl exec -it deployment/openab-cursor -- cursor-agent
   # invoke an MCP tool, approve the prompt; approval is saved
   ```

OpenAB itself auto-responds to ACP `session/request_permission` with `allow_always` (see `src/acp/connection.rs`), so once an MCP server is *loaded*, subsequent tool calls pass without prompting. The approval file only gates the initial load.

### Verifying MCP is loaded

```bash
kubectl exec deployment/openab-cursor -- cursor-agent mcp list
# Expected: "<server-name>: ready"
```

## Known Limitations

- Cursor Agent CLI is a separate distribution from Cursor Desktop — they are not the same binary
- No official apt/yum package; the Dockerfile downloads a pinned tarball directly
- `cursor-agent login` requires an interactive terminal for the device flow
- Auth token persistence requires a PVC mount at the user home directory

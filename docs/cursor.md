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

### Startup default: `--model auto`

`--model auto` in the `args` is load-bearing, not cosmetic. cursor-agent unconditionally overwrites its cli-config `selectedModel` at startup:

| startup `args` | post-startup default |
|---|---|
| `["acp"]` (no `--model`) | `composer-2[fast=true]` — Cursor's coding-only model, **not** Auto |
| `["acp", "--model", "auto"]` | `default[]` — true Auto (recommended) |
| `["acp", "--model", "<name>"]` | the matching modelId + parameters |

So **omit `--model auto` and every route lands on composer-2** regardless of what `/models` shows. `/models` switches only affect the live session; restarting the pod resets to whatever `args` dictates.

### At-runtime `/models` (Discord slash command)

Once the `openab-cursor` pod is running and a user has started a thread (by @mentioning the bot or any other trigger), three slash commands are available:

- `/models` — switch the model for the current channel's session
- `/agents` — switch agent mode (Agent / Plan / Ask)
- `/cancel` — interrupt the in-flight prompt

`/models` lists **all models the cursor-agent backend returns for the logged-in account** — the server-fetched list, not a baked-in allowlist. The list reflects account entitlements in real time, so a different Cursor account on the same binary can see a different list.

#### Soft rejects from unroutable models

Some modelIds (notably the `claude-*` family and parts of the `gpt-5.4`/`gpt-5.3-codex-spark` family on certain accounts) accept `session/set_config_option` with `setOk:true` but then stream a plain-text error on the very next `session/prompt`:

```
Error: I: AI Model Not Found
Model name is not valid: "<name>"
```

OpenAB **does not work around this** — per project policy, we trust the ACP protocol: `setOk:true` means the switch succeeded, so `/models` shows `✅ Switched to <name>`. The subsequent soft reject is passed through to the channel as the next turn's agent response. If you see that error, run `/models` again and pick Auto (`default[]`) or another routable model.

This is a cursor-agent ACP compliance gap tracked in [#493](https://github.com/openabdev/openab/issues/493); see also the Cursor forum thread linked there. Section **Known Limitations** below lists the full set of upstream issues.

#### Asking "who are you"

Cursor Auto routes to composer, which self-identifies as GPT. Thinking-mode models deliberate before replying. Treat `/models`'s followup message (derived from cursor-agent's structured `modelId`) as the source of truth, not the agent's self-identification.

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

### Packaging / auth
- Cursor Agent CLI is a separate distribution from Cursor Desktop — they are not the same binary
- No official apt/yum package; the Dockerfile downloads a pinned tarball directly
- `cursor-agent login` requires an interactive terminal for the device flow
- Auth token persistence requires a PVC mount at the user home directory

### Upstream cursor-agent ACP compliance gaps

The following are cursor-agent behaviours that violate ACP semantics. OpenAB does not work around them in-tree; they are tracked in [#493](https://github.com/openabdev/openab/issues/493) and reported upstream via the Cursor forum (link in that issue).

- **`setOk:true` for unroutable models** — `set_config_option` accepts models the account cannot route, surfacing the error one turn later as a plain-text soft reject (`AI Model Not Found` / `Model name is not valid`).
- **Unfiltered model list** — `session/new` returns 26 models on Pro accounts, of which ~11 are not routable for the logged-in account. The server knows entitlements and should filter.
- **`loadSession:true` capability is process-local** — `initialize` advertises `agentCapabilities.loadSession=true`, but `session/load` with an id from a previous cursor-agent process returns `-32602 Session not found`. Session IDs do not survive restarts, so pool-eviction and pod-restart both force a fresh session.

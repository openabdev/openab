# MiMoCode (mimo)

MiMoCode is a fork of OpenCode. It supports ACP over stdio and can be used as an OpenAB agent backend.

## Setup

| Field | Value |
|-------|-------|
| Image | `openab-mimocode` (or any image with `mimo` installed) |
| Command | `mimo` |
| Args | `["acp"]` |
| Working dir | `/home/node` |

## Authentication

MiMoCode offers a free tier (`MiMo Auto`) that requires no API key — just a one-time device auth:

```bash
mimo auth login --provider mimo --method "MiMo Auto (free)"
```

This is **fully non-interactive** and can be used in Dockerfiles, pre-boot hooks, or CI scripts. It sets `mimo/mimo-auto` as the default model (1M context, free).

The token expires in ~1 hour but auto-refreshes on next ACP session start. For persistent deployments, run this in a `[hooks.pre_boot]` script to ensure fresh auth on every container start.

## ⚠️ Important: SQLite DB Locking

MiMoCode uses a SQLite database (`~/.local/share/mimocode/mimocode.db`) for state. **Only one process can access it at a time.**

**Do NOT** run manual `mimo` commands (e.g. `mimo auth login`, `mimo debug config`, `mimo models`) while `mimo acp` is actively handling a request. This will corrupt or lock the database, causing all subsequent ACP requests to fail with empty responses or "Connection Lost".

### Safe workflow:
1. Start the bot (openab spawns `mimo acp` on first message)
2. Auth **before** the first message, or while the session is idle
3. If the DB gets corrupted:
   ```bash
   # As root:
   rm -f ~/.local/share/mimocode/mimocode.db*
   chown -R node:node ~/.local/share/mimocode/
   # As node:
   mimo auth login
   ```

## AWS/Bedrock Auto-Detection

When running on AWS (ECS, EC2), MiMoCode auto-detects AWS credentials and registers `amazon-bedrock` as a provider. Combined with its `Provider.sort()` logic, ACP mode picks the "best" (paid) model — which fails silently with 0 tokens if you don't have a paid Xiaomi account.

### Solution: pre_boot hook

Add this to your `[hooks.pre_boot]` script:

```bash
# Write mimo config — disables Bedrock, sets free model as default
mkdir -p ~/.config/mimocode
echo '{"disabled_providers":["amazon-bedrock"],"model":"mimo/mimo-auto"}' > ~/.config/mimocode/config.json

# Provision free-tier auth (non-interactive, token refreshes each boot)
mimo auth login --provider mimo --method "MiMo Auto (free)" 2>/dev/null || true
```

This ensures:
1. Bedrock is never auto-detected
2. `mimo/mimo-auto` is the default model (not `xiaomi/mimo-v2.5-pro-ultraspeed`)
3. Fresh auth token every container start

For paid Xiaomi accounts, replace `mimo/mimo-auto` with your preferred model (e.g. `xiaomi/mimo-v2.5-pro`).

## Config (gist)

```toml
[agent]
command = "mimo"
args = ["acp"]
env = { GHPOOL_URL = "http://ghpool.openab.local:8080", PATH = "/home/node/bin:/usr/local/bin:/usr/bin:/bin" }
```

## Known Limitations

- `mimo acp` does not accept `--model` flag (unlike the TUI)
- Default model is set during `mimo auth login` and stored in the DB
- No `config set` CLI command — model selection is via auth flow only
- The `-m/--model` flag only works for TUI/run modes, not ACP

## Web Search

Since MiMo-Code 0.1.17, built-in web search is off by default. To turn it on, set `MIMOCODE_ENABLE_EXA=1` in `[agent].env`, or add a search MCP server.

## Downgrading

MiMo-Code 0.1.17 runs SQLite migrations on `~/.local/share/mimocode/mimocode.db`: it rebuilds the memory full-text index and removes session-share data. If you roll an image back to 0.1.16 on a persistent volume, the older CLI may fail on the migrated database. Back up the database before upgrading, or delete it after downgrading (see [SQLite DB Locking](#️-important-sqlite-db-locking)) and log in again.

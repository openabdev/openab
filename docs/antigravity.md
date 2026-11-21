# Google Antigravity CLI (agy)

The Antigravity image starts [Google Antigravity CLI's native ACP server](https://antigravity.google/docs/cli/acp) with `agy acp`. Native ACP accepts image attachments in Discord prompts and exposes model choices through `/models`.

## Configuration

The Antigravity image sets `OPENAB_AGENT_COMMAND="agy acp"`. You can also select the native server explicitly on a host with Antigravity CLI 1.5.0 or later.

```toml
[agent]
command = "agy"
args = ["acp"]
```

## Authentication

Sign in with Google from inside the container and follow the CLI's prompts.

```bash
kubectl exec -it deployment/openab-antigravity -- agy acp --login
```

Persist the agent's home directory to retain its sign-in and conversations across container replacements.

## Docker

```bash
docker build -f Dockerfile.antigravity -t openab-antigravity .
```

## Helm

```yaml
agents:
  antigravity:
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels: ["123456789"]
    agent:
      command: "agy"
      args: ["acp"]
      workingDir: "/home/agent"
    # image: leave empty — chart auto-resolves to openab:<tag>-antigravity
```

## Upgrade from the legacy adapter

**Breaking change for deployments that use the image default.** The legacy adapter's session IDs can return `trajectory not found` on the first prompt after a native `session/load`. Start a fresh Discord thread for native ACP. To continue existing adapter-backed threads, select the legacy adapter explicitly before upgrading. The new images still ship the wrapper.

```toml
[agent]
command = "agy-acp"
args = []
```

## Legacy adapter

`agy-acp` translates ACP requests into `agy -p` invocations. It records a mapping from its own session ID to the Antigravity conversation ID and uses that mapping to continue a conversation.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGY_WORKING_DIR` | Working directory for agy invocations | `/tmp` |
| `AGY_EXTRA_ARGS` | Extra arguments added to every `agy` prompt invocation (optional) | (none) |
| `OPENAB_TOOL_DISPLAY` | Controls filtering of intermediate thinking narration ("I will..."). `full` or unset keeps all output; `compact`/`none`/`off` drops leading narration-only parts. | `full` |

### Print timeout

`agy-acp` supplies `--print-timeout 20m` on every prompt invocation unless
`AGY_EXTRA_ARGS` includes `--print-timeout <duration>` or
`--print-timeout=<duration>`. This gives longer tasks more time before the CLI
ends the turn with `Error: timeout waiting for response`.

**Migration:** The adapter now uses a 20-minute timeout instead of inheriting
the CLI timeout (5 minutes in the version used to diagnose this issue). To
retain a 5-minute limit or choose another duration, configure it explicitly:

```toml
[agent.env]
AGY_EXTRA_ARGS = "--print-timeout 5m"
```

The timeout remains finite; tasks exceeding the configured duration can still
fail. This setting controls the CLI print timeout, not other OpenAB timeouts.

### Steering Files

agy reads `AGENTS.md` and `GEMINI.md` when it considers a directory a workspace:

1. `AGENTS.md` and `GEMINI.md` are loaded first and injected into the system prompt
2. agy does not disclose how it determines HOME as a workspace, but `--add-dir` explicitly adds a directory
3. agy-acp **automatically** passes `--add-dir <working_dir>` on every invocation — no configuration needed

Place your steering instructions in `/home/agent/AGENTS.md` or `/home/agent/GEMINI.md` — they will be read on every prompt as long as `working_dir` points to that directory.

### Limitations

- **Text prompts only**: the adapter passes ACP text blocks to `agy -p`.
- **No streaming**: `agy -p` returns the full response at once; the adapter sends it as a single `agent_message_chunk` notification.
- **Cancel is a no-op**: `agy -p` runs to completion; `session/cancel` acknowledges but cannot interrupt.

### Model Selection

The Discord `/models` dropdown is supported. `agy-acp` returns model
`configOptions` when a session is created or loaded. It fetches the available
models from `agy models` and caches a successful result. If the live fetch fails,
the adapter uses the cache and then falls back to this static list when no cache
is available:

- Gemini 3.5 Flash (Medium)
- Gemini 3.5 Flash (High)
- Gemini 3.5 Flash (Low)
- Gemini 3.1 Pro (Low)
- Gemini 3.1 Pro (High)

When a user selects a model with `/models`, OpenAB sends
`session/set_config_option`. `agy-acp` stores the selected model for that session
and adds `--model <model_id>` to subsequent `agy` invocations.

For tab-separated `agy models` output (`model_id<TAB>display name`), the dropdown
shows the display name and passes only the ID to `--model`. Older single-column
model names remain supported. Display-name selections and saved values are
resolved to IDs when the live or cached model list contains a matching name.
Cached rows and model values saved by older adapters are handled automatically;
no session reset or cache deletion is needed. Unknown values and single-column
fallback names are passed through for the installed CLI to validate; the adapter
does not guess IDs for models absent from the live or cached list.

To select a default model for every new session, set `default_config_options`:

```toml
[pool]
default_config_options = { model = "gemini-3.8-flash-high" }
```

Use a model ID from the first column of `agy models` on your installation.
Display names are also accepted when a matching ID is available in that list;
using the ID avoids relying on CLI display-name support or name matching.

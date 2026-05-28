# Google Antigravity CLI (agy)

OpenAB supports [Google Antigravity CLI](https://antigravity.google/) via the `agy-acp` adapter — a thin Rust binary that translates ACP JSON-RPC into `agy -p` invocations.

## How It Works

```
openab ──ACP JSON-RPC──► agy-acp ──spawns──► agy --add-dir /home/agent -p "prompt"
                                              agy --add-dir /home/agent --conversation <ID> -p "follow-up"
```

- First prompt in a session: `agy -p "text"`, then discovers the conversation ID
- Subsequent prompts: `agy --conversation <ID> -p "text"` (resumes specific conversation)
- Only the **delta** (new response) is sent back — previous turns are not repeated
- Full `<sender_context>` metadata is passed through to agy

## Configuration

```toml
[agent]
command = "agy-acp"
args = []
working_dir = "/home/agent"
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `AGY_WORKING_DIR` | Working directory for agy invocations | `/tmp` |
| `AGY_EXTRA_ARGS` | Extra arguments prepended to every `agy` invocation (optional) | (none) |
| `AGY_DEFAULT_MODEL` | Model to seed into Antigravity's native `settings.json` when no model is configured | `Gemini 3.5 Flash (Medium)` |

`agy` print mode does not currently expose a `--model` flag. Model selection is
therefore controlled through Antigravity's own settings file:

```text
~/.gemini/antigravity-cli/settings.json
```

On startup, `agy-acp` creates or repairs that file only when no model is already
configured. Existing operator-selected models are preserved. This gives fresh
OpenAB Antigravity pods a known-good default while still allowing users to change
the model through Antigravity itself.

## Steering Files

agy reads `AGENTS.md` and `GEMINI.md` when it considers a directory a workspace:

1. `AGENTS.md` and `GEMINI.md` are loaded first and injected into the system prompt
2. agy does not disclose how it determines HOME as a workspace, but `--add-dir` explicitly adds a directory
3. agy-acp **automatically** passes `--add-dir <working_dir>` on every invocation — no configuration needed

Place your steering instructions in `/home/agent/AGENTS.md` or `/home/agent/GEMINI.md` — they will be read on every prompt as long as `working_dir` points to that directory.

## Docker

```bash
docker build -f Dockerfile.antigravity -t openab-antigravity .
```

## Authentication

Antigravity CLI uses Google Sign-In (OAuth). Authenticate inside the container:

```bash
kubectl exec -it deployment/openab-antigravity -- /lib64/ld-linux-x86-64.so.2 /usr/local/bin/agy auth
```

Complete the device flow in your browser. Auth tokens persist in the PVC at `~/.gemini/`.

## Helm

```yaml
agents:
  antigravity:
    discord:
      botToken: "${DISCORD_BOT_TOKEN}"
      allowedChannels: ["123456789"]
    agent:
      command: "agy-acp"
      args: []
      workingDir: "/home/agent"
    image:
      repository: ghcr.io/openabdev/openab-antigravity
      tag: "latest"
```

## Limitations

- **No streaming**: `agy -p` returns the full response at once; the adapter sends it as a single `agent_message_chunk` notification.
- **Cancel is a no-op**: `agy -p` runs to completion; `session/cancel` acknowledges but cannot interrupt.

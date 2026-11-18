# Kiro CLI v3 engine and Kiro workflows

kiro-cli 3.x ships a second engine, selected with `--agent-engine=v3`. It changes how agents are
selected and adds Kiro workflows over ACP. openab supports both, and every setting below is
opt-in: with none of them set, a kiro-cli v2 bot behaves as before.

## What changes with v3

- Agents appear as ACP **modes**. The flags `--agent`, `--trust-all-tools` and `--model` are
  rejected, and v2 JSON agents (`~/.kiro/agents/*.json`) are not read.
- A custom agent is a Markdown file `~/.kiro/agents/<name>.md`. Select it with
  `[pool] default_config_options = { mode = "<name>" }`.
- A session is created in the default mode (`vibe`) and then switched. An unknown mode name is
  **not** an error: kiro-cli answers success and the session stays in `vibe` with its default tool
  set. Turn on `strict_default_config_options` so openab refuses such a session.
- Each `kiro-cli` process uses about 0.5 GB of memory, and openab starts one per session. Size
  the container and `[pool] max_sessions` for the peak (about 2 GiB for 4 sessions plus openab).

## Configuration

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent-engine=v3"]
# Optional, only for Kiro workflows:
session_new_meta = { kiro = { settings = { workflows = { enabled = true } } } }

[pool]
max_sessions = 2
default_config_options = { mode = "openab-bot" }
strict_default_config_options = true
```

Agent file `~/.kiro/agents/openab-bot.md` (in the bot's `HOME`):

```markdown
---
name: openab-bot
description: Chat bot agent
tools: ["*"]
includeMcpJson: false
---

You are a helpful assistant in a chat thread.
```

Restrict `tools` to what the bot needs. openab auto-approves every permission request, so the
tool list is the only limit on what the agent can do.

Create an empty user MCP config, `~/.kiro/settings/mcp.json`:

```json
{"mcpServers": {}}
```

`includeMcpJson: false` does not stop user-level MCP servers from starting at `session/new`, so
an empty file is the reliable way to run without them.

## What the openab settings do

| Setting | Effect |
|---------|--------|
| `[agent] session_new_meta` | Sent as the `_meta` field of the `session/new` request. kiro-cli only honours the workflows switch there (not in `initialize`). Not sent on `session/load`. |
| `[pool] strict_default_config_options` | After each `session/set_config_option`, compares the returned `currentValue` with the requested value and fails session creation on a mismatch or an error. In this mode the legacy slash-command fallback is not used, so only a value the agent reports counts. A response without that option is accepted, as is an alias given as the option's display name. Default `false`. |
| `[pool] workflow_tail_quiet_secs` / `workflow_tail_max_secs` | Bounds of the late-output relay below. |

## Workflows

With workflows enabled, a prompt can start a workflow and `session/prompt` returns **before** the
workflow ends. The summary arrives later as unsolicited message chunks of the same session. When
workflows are enabled openab:

1. **Filters step sessions.** Each workflow step runs in its own ACP session and streams
   `session/update` frames with a different `params.sessionId`. Those frames are dropped from
   the reply. Frames without a `sessionId` are never dropped. Permission requests from step
   sessions are still auto-answered.
2. **Relays the late summary.** If a workflow started for the session during the turn, openab
   keeps reading after delivering the reply and posts the late text as a new message in the same
   thread, prefixed `_Workflow update:_`. Text is posted after `workflow_tail_quiet_secs` without
   new output, so text that arrives after a long pause is still delivered. Thinking and tool
   activity count as output for this timer but are not posted. The relay ends when the connection
   closes, when the next prompt on the session starts, or at `workflow_tail_max_secs`, measured from
   the prompt response. If a run is still open at the cap, a warning message says that further
   output is not relayed. Setting `workflow_tail_max_secs = 0` disables the relay and keeps the filter.

Limits: openab buffers at most 256 KiB of late text per message (the rest is dropped with a note), and each post is bounded to 30 s. Only workflows started during a turn are tracked. A workflow that is still running when the next prompt starts has its later output delivered with that prompt, not as a workflow update. If the agent pauses for longer than the quiet time while a run is still open, the text before the pause is posted as its own message and later text starts a new one.
Late text that arrives after the next prompt started is part of that prompt's reply. The relay is
not used on the `acp` gateway platform. `session_new_meta` and `default_config_options` are
applied when a session is created with `session/new` only, not when a session is restored with `session/load`.

## Security notes

- Workflow steps run shell commands, and openab answers every permission request with
  "allow always". Each step session asks two to three times per command.
- Text from the chat reaches workflow steps as user text. Allow only fixed, reviewed workflow
  files, use a private channel, and keep step agents (`.md` files) to the minimum tool set.
- Custom agent `resources` are not loaded inside workflow steps.
- Upstream issues to know about: a workflow result can be lost when the parent session is busy,
  and a run can hang if a permission request is never answered. openab bounds the wait but does
  not fix either.

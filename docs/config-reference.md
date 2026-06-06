# Configuration Reference

OpenAB is configured via a TOML file (default: `config.toml`). Environment variables can be interpolated using `${VAR_NAME}` syntax.

At least one adapter section (`[discord]` or `[slack]`) is required.

## Loading Config

Specify the config source with `--config` / `-c`:

```bash
# Local file (default: config.toml when omitted)
openab run -c config.toml

# Remote URL via HTTPS (recommended)
openab run -c https://example.com/config.toml

# Remote URL via HTTP (warns — avoid in production; config contains secrets)
openab run -c http://internal.example.com/config.toml
```

Remote config is fetched via HTTP GET with a 10-second timeout and a 1 MiB response size limit. Environment variable expansion (`${VAR}`) works identically on both local and remote config content.

> **Security best practice:** Never hardcode secrets in remote config files. Use environment variable references like `bot_token = "${DISCORD_BOT_TOKEN}"` and inject the actual values via local environment variables or Kubernetes Secrets. OpenAB expands `${VAR}` identically for both local and remote config.

---

## `[discord]`

Discord adapter. Requires a Discord bot token.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bot_token` | string | *required* | Discord bot token. Use `${DISCORD_BOT_TOKEN}` for env var. |
| `allow_all_channels` | bool \| omit | auto-detect | `true` = all channels; `false` = only `allowed_channels`. Omitted = inferred from list (non-empty → false, empty → true). |
| `allowed_channels` | string[] | `[]` | Channel IDs to allow. Only checked when `allow_all_channels` resolves to false. |
| `allow_all_users` | bool \| omit | auto-detect | `true` = any user; `false` = only `allowed_users`. Omitted = inferred from list. |
| `allowed_users` | string[] | `[]` | User IDs to allow. Only checked when `allow_all_users` resolves to false. |
| `allow_bot_messages` | string | `"off"` | `"off"` — ignore all bot messages. `"mentions"` — only process bot messages that @mention this bot. `"all"` — process all bot messages (capped by `max_bot_turns`). |
| `trusted_bot_ids` | string[] | `[]` | When non-empty, only these bot IDs pass the bot gate. Empty = any bot (mode permitting). **Admission override:** a trusted bot that @mentions this bot bypasses `allow_bot_messages` mode entirely (treated as human @mention, can pull bot into threads). |
| `allow_user_messages` | string | `"involved"` | `"involved"` — reply in threads bot has participated in without @mention; channel messages require @mention; DMs always process. `"mentions"` — always require @mention. `"multibot-mentions"` — like `"involved"`, but require @mention once another bot has posted in the thread. |
| `allow_dm` | bool | `false` | `true` = respond to Discord DMs; `false` = ignore DMs. `allowed_users` still applies in DMs. Each DM user consumes one session slot. |
| `max_bot_turns` | u32 | `100` | Max consecutive bot turns per thread before throttling (soft limit). Human message resets the counter. A compiled-in hard cap of 1000 consecutive bot messages is always enforced. |
| `message_processing_mode` | string | `"per-message"` | Message dispatch mode: `"per-message"` (each message = own turn), `"per-thread"` (all messages in thread share one buffer), or `"per-lane"` (each sender gets own buffer). See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Per-thread/lane mpsc channel capacity. Only applies to `per-thread` / `per-lane` modes. |
| `max_batch_tokens` | u32 | `24000` | Soft token cap per ACP turn. Only applies to `per-thread` / `per-lane` modes. |

---

## `[slack]`

Slack adapter using Socket Mode. Requires both a Bot User OAuth Token and an App-Level Token.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `bot_token` | string | *required* | Bot User OAuth Token (`xoxb-...`). |
| `app_token` | string | *required* | App-Level Token (`xapp-...`) for Socket Mode. |
| `allow_all_channels` | bool \| omit | auto-detect | Same behavior as Discord. |
| `allowed_channels` | string[] | `[]` | Slack channel IDs (e.g. `C0123456789`). |
| `allow_all_users` | bool \| omit | auto-detect | Same behavior as Discord. |
| `allowed_users` | string[] | `[]` | Slack user IDs (e.g. `U0123456789`). |
| `allow_bot_messages` | string | `"off"` | Same as Discord. |
| `trusted_bot_ids` | string[] | `[]` | Slack Bot User IDs (`U...`) or Bot IDs (`B...`). `U...` matching resolves event Bot IDs via Slack `bots.info`, so the bot token needs `users:read`. |
| `allow_user_messages` | string | `"involved"` | Same as Discord. |
| `max_bot_turns` | u32 | `100` | Same as Discord. |
| `message_processing_mode` | string | `"per-message"` | Same as Discord. See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Same as Discord. |
| `max_batch_tokens` | u32 | `24000` | Same as Discord. |

---

## `[gateway]`

Custom Gateway adapter for platforms like Telegram, LINE, Feishu/Lark, and Google Chat. Connects to the gateway via WebSocket.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `url` | string | *required* | WebSocket URL of the gateway (e.g. `ws://openab-gateway:8080/ws`). |
| `platform` | string | `"telegram"` | Platform name for session key namespacing (e.g. `"telegram"`, `"line"`, `"feishu"`, `"googlechat"`). |
| `token` | string | — | Shared token for WebSocket authentication (optional but recommended). |
| `bot_username` | string | — | Bot username for @mention gating in groups. |
| `allow_all_channels` | bool \| omit | auto-detect | `true` = all channels; `false` = only `allowed_channels`. Omitted = inferred from list (non-empty → false, empty → true). |
| `allowed_channels` | string[] | `[]` | Chat/group IDs to allow. Only checked when `allow_all_channels` resolves to false. |
| `allow_all_users` | bool \| omit | auto-detect | `true` = any user; `false` = only `allowed_users`. Omitted = inferred from list. |
| `allowed_users` | string[] | `[]` | User IDs to allow. Only checked when `allow_all_users` resolves to false. |
| `message_processing_mode` | string | `"per-message"` | Same as Discord. See [Message Dispatch Modes](message-dispatch-modes.md). |
| `max_buffered_messages` | u32 | `10` | Same as Discord. |
| `max_batch_tokens` | u32 | `24000` | Same as Discord. |

---

## `[agent]`

The AI agent subprocess that OpenAB spawns to handle messages via ACP.

> **This entire section is optional.** If omitted, `command` and `args` default from `$OPENAB_AGENT_COMMAND` (e.g. `"opencode acp"` — first token is command, rest are args). Each Docker image sets this env var so you typically don't need an `[agent]` block unless you want to override `env` or `args`.

**Resolution priority:** config `[agent].command`/`args` > `$OPENAB_AGENT_COMMAND` > `"openab-agent"`

> **Partial override rule:** Setting `command` without `args` resets args to `[]`. This prevents a custom command from inheriting the env var's args. To keep env-var args with a custom command, set both fields explicitly.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `command` | string | from `$OPENAB_AGENT_COMMAND` or `"openab-agent"` | Agent binary. Optional — defaults from image env var. |
| `args` | string[] | from `$OPENAB_AGENT_COMMAND` or `[]` | CLI arguments. Defaults to env var args only when `command` is also defaulted. |
| `working_dir` | string | `$HOME` | Working directory for the agent process. Optional — defaults to container's `$HOME`. |
| `env` | map | `{}` | Extra environment variables (e.g. `{ OPENAI_API_KEY = "${OPENAI_API_KEY}" }`). |
| `inherit_env` | string[] | `[]` | Env var names to inherit from the OAB process (e.g. vars injected via K8s `envFrom`). Keys in `env` take precedence. |

> **Default inherited vars:** After `env_clear()`, the agent always receives `HOME`, `PATH`, and `USER` (on Windows: `USERPROFILE`, `USERNAME`, `PATH`, `SystemRoot`, `SystemDrive`). Use `inherit_env` to pass additional vars beyond this baseline.

### Authentication

Each image sets `OPENAB_AGENT_AUTH_COMMAND` with the correct auth command. To authenticate any agent:

```bash
kubectl exec -it deployment/openab-<name> -- sh -c "$OPENAB_AGENT_AUTH_COMMAND"
```

This works for all agents regardless of backend — no need to remember the specific auth command.

### Agent examples

```toml
# Kiro CLI
[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

# Claude Code
[agent]
command = "claude-agent-acp"
args = []
working_dir = "/home/node"
# Auth: kubectl exec -it deploy/openab-claude -- claude auth login
# Credentials persist in HOME PVC across restarts. See docs/claude-code.md.

# Codex
[agent]
command = "codex"
args = ["--acp"]
working_dir = "/home/node"
env = { OPENAI_API_KEY = "${OPENAI_API_KEY}" }

# Gemini CLI
[agent]
command = "gemini"
args = ["--acp"]
working_dir = "/home/node"
env = { GEMINI_API_KEY = "${GEMINI_API_KEY}" }

# GitHub Copilot
[agent]
command = "copilot"
args = ["--acp", "--stdio"]
working_dir = "/home/node"

# opencode
[agent]
command = "opencode"
args = ["acp"]
working_dir = "/home/node"

# Pi Agent
[agent]
command = "pi-acp"
working_dir = "/home/node"

# Cursor Agent
[agent]
command = "cursor-agent"
args = ["acp", "--model", "auto", "--workspace", "/home/agent"]
working_dir = "/home/agent"

# Hermes Agent
[agent]
command = "hermes-acp"
working_dir = "/home/agent"
```

---

## `[pool]`

Session pool settings for managing concurrent agent sessions.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_sessions` | usize | `10` | Maximum number of concurrent agent sessions. When full, the oldest idle session is suspended (recoverable); if all sessions are busy, new requests are rejected. |
| `session_ttl_hours` | u64 | `4` | Session time-to-live in hours. Idle sessions are reclaimed after this period. The example config uses `24`. |

---

## `[hooks]`

Lifecycle hooks that run custom scripts at specific points during the container lifecycle. See [hooks.md](hooks.md) for full documentation and examples.

### `[hooks.pre_boot]`

Runs **before** agent pool creation. Use for bootstrapping files, syncing from S3, installing CLIs.

### `[hooks.pre_shutdown]`

Runs **after** pool shutdown on SIGTERM. Use for backing up state, syncing to S3.

Both hooks share the same fields:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `script` | string | — | Absolute path to an executable script. |
| `inline` | string | — | Script content (written to temp file and executed). |
| `url` | string | — | Remote script URL (max 1 MiB). |
| `sha256` | string | — | Required with `url` — hex-encoded SHA-256 checksum. |
| `timeout_seconds` | u64 | `60` | Max wall-clock seconds before the script is killed. |
| `on_failure` | string | `"abort"` | `"abort"` exits openab; `"warn"` logs and continues. |

> Exactly one of `script`, `inline`, or `url` must be set. `script` must be an absolute path. `url` requires `sha256`.

```toml
[hooks.pre_boot]
inline = '''
#!/bin/sh
set -e
aws s3 sync "$BOOTSTRAP_URI" "$HOME/"
'''
timeout_seconds = 120
on_failure = "abort"

[hooks.pre_shutdown]
inline = '''
#!/bin/sh
aws s3 sync "$HOME/" "s3://$STATE_BUCKET/$TASK_FAMILY/" \
  --exclude "aws-cli/*" --quiet
'''
timeout_seconds = 30
on_failure = "warn"
```

---

## `[reactions]`

Emoji reaction feedback on messages to show agent processing status.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable/disable reaction feedback. |
| `remove_after_reply` | bool | `false` | Remove the status reaction after the agent replies. |
| `tool_display` | string | `"full"` | How tool calls are rendered: `"full"` (complete title), `"compact"` (count summary, e.g. `✅ 3 · 🔧 1 tool(s)`), or `"none"` (hidden). |

### `[reactions.emojis]`

Customize the emoji for each processing stage.

| Key | Default | Description |
|-----|---------|-------------|
| `queued` | 👀 | Message received, queued for processing. |
| `thinking` | 🤔 | Agent is thinking / generating. |
| `tool` | 🔥 | Agent is calling a tool. |
| `coding` | 👨‍💻 | Agent is writing code. |
| `web` | ⚡ | Agent is doing web operations. |
| `done` | 🆗 | Agent finished successfully. |
| `error` | 😱 | Agent encountered an error. |

### `[reactions.timing]`

Fine-tune reaction timing behavior (milliseconds).

| Key | Default | Description |
|-----|---------|-------------|
| `debounce_ms` | `700` | Debounce interval before updating the reaction emoji. |
| `stall_soft_ms` | `10000` | Soft stall threshold — warn if no progress. |
| `stall_hard_ms` | `30000` | Hard stall threshold — consider the agent stuck. |
| `done_hold_ms` | `1500` | How long to show the done emoji before removing (if `remove_after_reply`). |
| `error_hold_ms` | `2500` | How long to show the error emoji before removing. |

---

## `[stt]`

Speech-to-text transcription for voice messages. Uses an OpenAI-compatible `/audio/transcriptions` endpoint.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Enable voice message transcription. |
| `api_key` | string | `""` | API key for the STT service. When empty and `base_url` contains `groq.com`, the `GROQ_API_KEY` environment variable is used automatically. For local servers, use `api_key = "not-needed"`. |
| `model` | string | `"whisper-large-v3-turbo"` | Model name to use for transcription. |
| `base_url` | string | `"https://api.groq.com/openai/v1"` | Base URL of the STT API. Any OpenAI-compatible `/audio/transcriptions` endpoint works. |
| `echo_transcript` | bool | `false` | When set to `true` and STT runs, post a `> 🎤 <transcript>` message to the thread before the agent reply so users can verify what was heard. Failures show `(transcription failed)` and add a ⚠️ reaction to the original message. |

---

## `[cron]`

Everything cron-related lives under `[cron]`.

```toml
[cron]
usercron_enabled = true                      # enable hot-reload (default: false)
usercron_path = "cronjob.toml"               # relative to $HOME/.openab/, or absolute

[[cron.jobs]]
enabled = true                               # optional, default: true
schedule = "0 9 * * 1-5"                    # cron expression (5-field POSIX)
channel = "123456789"                        # target channel/thread ID
message = "summarize yesterday's merged PRs" # message sent to agent
platform = "discord"                         # optional, default: "discord"
sender_name = "DailyOps"                     # optional, default: "openab-cron"
timezone = "America/New_York"                # optional, default: "UTC"
thread_id = ""                               # optional, post to existing thread

[[cron.jobs]]
schedule = "0 0 * * 0"
channel = "123456789"
message = "generate weekly status report"
platform = "discord"
timezone = "UTC"
```

### `[cron]` fields

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `usercron_enabled` | bool | `false` | Enable usercron hot-reload. Must be explicitly set to `true`. |
| `usercron_path` | string | — | Path to the external `cronjob.toml`. Relative paths resolve from `$HOME/.openab/`. |

### `[[cron.jobs]]` fields

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Set `false` to disable without removing the entry. |
| `schedule` | string | *required* | Cron expression (minute, hour, day-of-month, month, day-of-week). |
| `channel` | string | *required* | Target Discord channel/thread ID or Slack channel ID. |
| `message` | string | *required* | Message sent to the agent as a prompt. |
| `platform` | string | `"discord"` | Target platform (`"discord"` or `"slack"`). |
| `sender_name` | string | `"openab-cron"` | Sender attribution shown in the prompt context. |
| `timezone` | string | `"UTC"` | IANA timezone for schedule evaluation (e.g. `"America/New_York"`, `"Europe/Berlin"`). |
| `thread_id` | string | `""` | Optional thread ID to post into an existing thread. |

The external `cronjob.toml` uses `[[jobs]]` (same fields). See [Usercron docs](cronjob.md#usercron--hot-reload-with-cronjobtoml) for details.

### Usercron-only `[[jobs]]` fields

These fields are valid only in the external usercron file, for example `$HOME/.openab/cronjob.toml`. They are rejected in baseline `[[cron.jobs]]` because OpenAB only writes state back to the user-managed cron file.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `id` | string | *required with `disable_on_success`* | Stable job ID used when the scheduler writes `enabled = false` or `thread_id` back to `cronjob.toml`. |
| `disable_on_success` | string | — | Command to run before sending the scheduled prompt. |
| `disable_on_success_match` | string | *required with `disable_on_success`* | Marker that must appear in stdout or stderr, in addition to exit code `0`, before the job is considered complete. |
| `disable_on_success_timeout_secs` | integer | `60` | Timeout for the completion check command. |
| `disable_on_success_working_dir` | string | — | Working directory for the completion check command. |

Example:

```toml
[[jobs]]
id = "fix-unit-tests"
enabled = true
schedule = "*/10 * * * *"
channel = "123456789"
message = "Unit tests are still failing. Continue fixing them."
disable_on_success = "npm test && echo OPENAB_GOAL_SUCCESS"
disable_on_success_match = "OPENAB_GOAL_SUCCESS"
disable_on_success_timeout_secs = 120
disable_on_success_working_dir = "/workspace/my-project"
```

**Cron expression format:**

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-7, 0 and 7 = Sunday)
│ │ │ │ │
* * * * *
```

**Behaviors:**
- Scheduler evaluates expressions once per minute
- If a previous execution is still running, the next tick is skipped (no overlap)
- Failed executions are logged but do not block other jobs or chat traffic
- Stateless — no persistence needed, re-evaluated from config on restart

---

## Customizing via Helm

When deploying with the Helm chart (`charts/openab`), the `config.toml` is generated from `values.yaml`. Each agent is defined under the `agents` map:

```yaml
agents:
  kiro:
    command: kiro-cli
    args: ["acp", "--trust-all-tools"]
    discord:
      enabled: true
      allowedChannels: ["1234567890"]
      allowBotMessages: "mentions"
      trustedBotIds: ["9876543210"]
    pool:
      maxSessions: 10
      sessionTtlHours: 24
    reactions:
      enabled: true
    stt:
      enabled: true
      apiKey: "your-groq-key"
```

Key mapping (`values.yaml` → `config.toml`):

| Helm value | Config key |
|---|---|
| `agents.<name>.discord.allowedChannels` | `[discord] allowed_channels` |
| `agents.<name>.discord.allowBotMessages` | `[discord] allow_bot_messages` |
| `agents.<name>.discord.trustedBotIds` | `[discord] trusted_bot_ids` |
| `agents.<name>.discord.allowUserMessages` | `[discord] allow_user_messages` |
| `agents.<name>.discord.messageProcessingMode` | `[discord] message_processing_mode` |
| `agents.<name>.discord.maxBufferedMessages` | `[discord] max_buffered_messages` |
| `agents.<name>.discord.maxBatchTokens` | `[discord] max_batch_tokens` |
| `agents.<name>.slack.*` | `[slack] *` (same pattern) |
| `agents.<name>.pool.maxSessions` | `[pool] max_sessions` |
| `agents.<name>.pool.sessionTtlHours` | `[pool] session_ttl_hours` |
| `agents.<name>.reactions.enabled` | `[reactions] enabled` |
| `agents.<name>.reactions.toolDisplay` | `[reactions] tool_display` |
| `agents.<name>.stt.apiKey` | `[stt] api_key` |
| `agents.<name>.cronjobs[].enabled` | `[[cron.jobs]] enabled` |
| `agents.<name>.cronjobs[].schedule` | `[[cron.jobs]] schedule` |
| `agents.<name>.cronjobs[].channel` | `[[cron.jobs]] channel` |
| `agents.<name>.cronjobs[].message` | `[[cron.jobs]] message` |
| `agents.<name>.cronjobs[].platform` | `[[cron.jobs]] platform` |
| `agents.<name>.cronjobs[].senderName` | `[[cron.jobs]] sender_name` |
| `agents.<name>.cronjobs[].timezone` | `[[cron.jobs]] timezone` |
| `agents.<name>.cronjobs[].threadId` | `[[cron.jobs]] thread_id` |

> ⚠️ Use `--set-string` (not `--set`) for Discord/Slack IDs to avoid float64 precision loss:
> ```bash
> helm upgrade --install mybot charts/openab \
>   --set-string agents.kiro.discord.allowedChannels[0]="1234567890"
> ```

See `charts/openab/values.yaml` for the full list of Helm values including `persistence`, `image`, `resources`, and multi-agent examples.

---

## Environment variable interpolation

Any value can reference environment variables with `${VAR_NAME}`:

```toml
bot_token = "${DISCORD_BOT_TOKEN}"
```

Undefined variables resolve to an empty string.

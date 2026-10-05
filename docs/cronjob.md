# Scheduled Messages (Config-Driven Cron)

Send recurring prompts to your agent on a schedule — daily summaries, weekly reports, periodic scans — without external infrastructure.

## How It Works

1. Define `[[cron.jobs]]` entries in `config.toml`
2. OpenAB's internal scheduler evaluates cron expressions once per minute
3. When a schedule matches, the message is sent to the agent as if a user typed it
4. The agent processes the message and replies to the target channel

No external scheduler (K8s CronJob, GitHub Actions) is needed for simple use cases.

## Quick Start

Add to your `config.toml`:

```toml
[[cron.jobs]]
schedule = "0 9 * * 1-5"
channel = "123456789012345678"
message = "summarize yesterday's merged PRs"
```

This sends `summarize yesterday's merged PRs` to the agent every weekday at 09:00 UTC in the specified Discord channel.

## Configuration

Each `[[cron.jobs]]` entry supports these fields:

```toml
[[cron.jobs]]
enabled = true                               # optional, default: true
schedule = "0 9 * * 1-5"                    # required: cron expression
channel = "123456789012345678"               # required: target channel ID
message = "summarize yesterday's merged PRs" # required: prompt for the agent
platform = "discord"                         # optional, default: "discord"
sender_name = "DailyOps"                     # optional, default: "openab-cron"
timezone = "America/New_York"                     # optional, default: "UTC"
thread_id = ""                               # optional: post to existing thread
```

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `enabled` | | `true` | Set `false` to disable without removing the entry |
| `schedule` | ✅ | — | 5-field POSIX cron expression |
| `channel` | ✅ | — | Discord channel/thread ID, Slack channel ID, Telegram chat ID, Google Chat space name, or LINE WORKS channel ID / `user:<userId>` |
| `message` | ✅ | — | Message sent to the agent as a prompt |
| `platform` | | `"discord"` | `"discord"`, `"slack"`, `"telegram"`, `"googlechat"`, or `"lineworks"` (non-default platforms require their feature) |
| `sender_name` | | `"openab-cron"` | Attribution shown in prompt context |
| `timezone` | | `"UTC"` | IANA timezone (e.g. `"America/New_York"`, `"Europe/Berlin"`) |
| `thread_id` | | — | Post into an existing thread instead of the channel |

## Cron Expression Format

Standard 5-field POSIX cron, same as Linux crontab, K8s CronJob, and GitHub Actions:

```
┌───────────── minute (0-59)
│ ┌───────────── hour (0-23)
│ │ ┌───────────── day of month (1-31)
│ │ │ ┌───────────── month (1-12)
│ │ │ │ ┌───────────── day of week (0-7, 0 and 7 = Sunday)
│ │ │ │ │
* * * * *
```

### Examples

| Expression | Meaning |
|---|---|
| `0 9 * * 1-5` | Weekdays at 09:00 |
| `0 0 * * 0` | Sundays at midnight |
| `*/30 * * * *` | Every 30 minutes |
| `0 18 * * 1-5` | Weekdays at 18:00 |
| `0 9 1 * *` | First day of every month at 09:00 |

## Timezone Support

By default, schedules are evaluated in UTC. Set `timezone` to any IANA timezone:

```toml
[[cron.jobs]]
schedule = "0 9 * * 1-5"
channel = "123456789012345678"
message = "good morning team, here's today's agenda"
timezone = "America/New_York"
```

This fires at 09:00 New York time (13:00 or 14:00 UTC depending on DST).

## Multiple Jobs

Define as many `[[cron.jobs]]` entries as you need:

```toml
[[cron.jobs]]
schedule = "0 9 * * 1-5"
channel = "123456789012345678"
message = "summarize yesterday's merged PRs"
sender_name = "DailyOps"
timezone = "America/New_York"

[[cron.jobs]]
schedule = "0 0 * * 0"
channel = "123456789012345678"
message = "generate weekly status report"
sender_name = "WeeklyReport"

[[cron.jobs]]
schedule = "0 18 * * 1-5"
channel = "C0123456789"
message = "check for any critical alerts in the last 8 hours"
platform = "slack"
sender_name = "OpsBot"

[[cron.jobs]]
schedule = "* * * * *"
channel = "176096071"
message = "講一個冷笑話"
platform = "telegram"
sender_name = "JokeBot"

[[cron.jobs]]
schedule = "0 9 * * 1-5"
channel = "spaces/AAAA1234567"
message = "summarize the new support escalations"
platform = "googlechat"
sender_name = "SupportDigest"
timezone = "Asia/Taipei"
```

## Helm Deployment

When using the Helm chart, define cronjobs under each agent in `values.yaml`:

```yaml
agents:
  kiro:
    cronjobs:
      - schedule: "0 9 * * 1-5"
        channel: "123456789012345678"
        message: "summarize yesterday's merged PRs"
        platform: "discord"
        senderName: "DailyOps"
        timezone: "America/New_York"
      - schedule: "0 0 * * 0"
        channel: "123456789012345678"
        message: "generate weekly status report"
```

> ⚠️ Use `--set-string` for channel IDs to avoid float64 precision loss:
> ```bash
> helm upgrade mybot charts/openab \
>   --set-string agents.kiro.cronjobs[0].channel="123456789012345678"
> ```

## Usercron — Hot-Reload with `cronjob.toml`

Cronjobs defined in `config.toml` require a redeploy to change. **Usercron** lets you manage schedules in a separate `cronjob.toml` file that the scheduler hot-reloads automatically — no restart needed.

### Enable Usercron

Add to your `config.toml`:

```toml
[cron]
usercron_enabled = true
usercron_path = "cronjob.toml"
```

Usercron is **disabled by default**. Both fields are required to activate it.

#### Minimal config.toml example

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"

[agent]
command = "kiro-cli"
args = ["acp", "--trust-all-tools"]
working_dir = "/home/agent"

[cron]
usercron_enabled = true
usercron_path = "cronjob.toml"    # → $HOME/.openab/cronjob.toml
```

> Note: Everything cron-related lives under `[cron]` — both usercron settings and baseline `[[cron.jobs]]`.

The path is relative to `$HOME/.openab/` (e.g. `"cronjob.toml"` resolves to `$HOME/.openab/cronjob.toml`). Absolute paths are used as-is. The scheduler starts watching immediately, even if the file doesn't exist yet.

> **New installations**: If `~/.openab/` does not exist yet, the scheduler silently skips the file and continues running. Once you create the directory and place `cronjob.toml` inside, it will be picked up automatically on the next tick — no restart required.

> [!CAUTION]
> **Breaking Change (v0.8.2)** — `usercron_path` relative path base changed from `$HOME` to `$HOME/.openab/`.
> If you are upgrading from a previous version, move your existing file:
> ```bash
> mkdir -p ~/.openab
> mv ~/cronjob.toml ~/.openab/cronjob.toml
> ```

### Create `cronjob.toml`

Same format as `[[cron.jobs]]` in config.toml, but uses `[[jobs]]`:

```toml
[[jobs]]
schedule = "* * * * *"
channel = "1490282656913559673"
message = "ping"
platform = "discord"
sender_name = "usercron"
timezone = "Asia/Taipei"

[[jobs]]
schedule = "0 9 * * 1-5"
channel = "1490282656913559673"
message = "summarize yesterday's merged PRs"
sender_name = "DailyOps"
timezone = "Asia/Taipei"
```

### How It Works

```
                         config.toml                   $HOME/.openab/cronjob.toml
                    ┌──────────────────┐                 ┌──────────────────────┐
                    │ [cron]           │                 │ [[jobs]]             │
                    │ usercron_enabled │                 │ schedule = "* * * *" │
                    │   = true         │                 │ channel  = "123..."  │
                    │ usercron_path    │                 │ message  = "ping"    │
                    │   = "cronjob.toml│"                └──────────┬───────────┘
                    │                  │                            │
                    │ [[cron.jobs]]    │                   Agent writes here
                    │ (baseline jobs)  │                   anytime (mobile/CLI)
                    └────────┬─────────┘                           │
                             │                                     │
                    ┌────────▼─────────┐                           │
                    │  OAB Scheduler   │◄──────────────────────────┘
                    │  (ticks every    │   check mtime every tick
                    │   1 minute)      │   reload if changed
                    └────────┬─────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
     baseline jobs    usercron jobs    should_fire()?
     (immutable)      (hot-reload)         │
              │              │         ┌────▼────┐
              └──────────────┘    no── │ match?  │ ──yes──► fire_cronjob()
                                      └─────────┘          → send message
                                                            → create thread
                                                            → agent processes
```

1. Every scheduler tick (~1 minute), the file's modification time is checked
2. If the file changed → re-parse and replace the dynamic job list
3. `config.toml` `[[cron.jobs]]` are the **immutable baseline**; `cronjob.toml` jobs are the **dynamic overlay**
4. Invalid TOML or bad entries are logged and skipped — baseline jobs are never affected
5. Deleting the file removes all dynamic jobs (baseline jobs continue)

### Agent-Managed Schedules

Because `cronjob.toml` is a plain file, your agent can write to it directly:

```
User: set up a cronjob that pings me every minute
Agent: ✅ Written to cronjob.toml, takes effect within 1 minute
```

This enables mobile-friendly schedule management — talk to your agent from your phone, and it updates the cron file for you.

### Goal-Driven Auto-Disable

Usercron jobs can stop themselves once a goal is complete. Add `disable_on_success` to run a command before the scheduled prompt is sent. The job is considered complete only when the command exits `0` **and** stdout or stderr contains `disable_on_success_match`.

```toml
[[jobs]]
id = "fix-unit-tests"                       # required for scheduler writeback
enabled = true
schedule = "*/10 * * * *"
channel = "1490282656913559673"
message = "Unit tests are still failing. Continue fixing them and report progress."

disable_on_success = "npm test && echo OPENAB_GOAL_SUCCESS"
disable_on_success_match = "OPENAB_GOAL_SUCCESS"
disable_on_success_timeout_secs = 120
disable_on_success_working_dir = "/workspace/my-project"
```

Execution flow:

1. The schedule matches.
2. The scheduler runs `disable_on_success`.
3. If the command exits `0` and output contains `disable_on_success_match`, OpenAB posts `✅ Goal achieved`, writes `enabled = false` back to the usercron file (`usercron_path`, `$HOME/.openab/cronjob.toml` by default), and skips the regular prompt.
4. Otherwise, OpenAB sends the regular `message` and the agent continues working.

`disable_on_success` is supported only in usercron `[[jobs]]`, not baseline `[[cron.jobs]]`. This keeps scheduler writeback limited to the user-managed cron file.

> [!CAUTION]
> `disable_on_success` is run by OpenAB as its own process user, outside the agent's tool approval. Anyone who can write `cronjob.toml`, including the agent, can schedule shell commands this way. See [Autonomy and Permission Scope](#autonomy-and-permission-scope).

### Re-enabling a Disabled Job

Once a goal is achieved and the job is disabled, re-enable it by editing the usercron file (`$HOME/.openab/cronjob.toml` by default, or the resolved `usercron_path`):

```toml
# Flip back to true to restart the job
enabled = true
```

This can be done manually, or by asking the AI agent (e.g. "re-enable the fix-unit-tests cron job").

### Kubernetes Deployment

Mount `cronjob.toml` on a PVC so it persists across pod restarts, and set `usercron_path` in your config.toml:

```toml
# config.toml
[cron]
usercron_enabled = true
# Relative to $HOME/.openab/ — resolves to $HOME/.openab/cronjob.toml
usercron_path = "cronjob.toml"
```

## Autonomy and Permission Scope

Cron is how OpenAB gives an agent a degree of autonomy: it can act on a schedule without a human in the loop, and with [Agent-Managed Schedules](#agent-managed-schedules) it can decide its own schedule. This section describes what that capability covers, so you can decide how much of it to grant.

### Cron prompts skip inbound chat checks (by design)

Cron jobs are system-initiated. When a job fires, the scheduler hands the prompt directly to the message router, bypassing the platform adapter's inbound event path (replies are still sent through the adapter). A cron prompt therefore never enters the adapter's event handler, and none of the adapter's inbound filters apply to it. For example:

- user, channel, and role allowlists (`allowed_users`, `allowed_channels`, and similar)
- DM gating (`allow_dm`, where supported)
- @mention requirements
- bot-message filtering

This is intentional: scheduled jobs have no external sender and may target channels outside the human-facing allowlist (see the [Identity Trust-None ADR](adr/identity-trust-none.md)). The prompt runs with the same tool permissions as any other session. OpenAB forwards the message text verbatim and does not interpret it; depending on the agent CLI, text such as `/clear` may be handled as a command (for example, kiro-cli does).

### What write access to `cronjob.toml` grants

Baseline `[[cron.jobs]]` in `config.toml` can only send prompts. Usercron `[[jobs]]` in `cronjob.toml` can also run a shell command through [`disable_on_success`](#goal-driven-auto-disable), and with Agent-Managed Schedules the agent itself can write `cronjob.toml`.

`disable_on_success` is executed by OpenAB itself (`sh -c` on Unix, `cmd /C` on Windows) on every schedule match, before the prompt is sent. A usercron job that sets `disable_on_success` must also set a non-empty `id` and `disable_on_success_match`. If either is missing, the whole entry is dropped at load time with only a warning in the logs, and the job never fires. Jobs that do not set `disable_on_success` do not need `id` for this.

The command runs as the OpenAB process user, outside the agent's tool approval and any agent sandbox. It inherits OpenAB's filesystem, network access, mounts, and full environment, which often includes platform bot tokens (for example `bot_token = "${DISCORD_BOT_TOKEN}"`) and other credentials. The agent's own process does not get these: OpenAB clears the environment when it starts the agent and passes through only a few baseline variables plus `[agent].env` and `inherit_env`. Run OpenAB under a dedicated account, and give its environment and mounts only the credentials OpenAB itself needs.

How much this adds depends on your setup:

- If the agent already has unrestricted shell access (for example `--trust-all-tools`, as in the [example above](#minimal-configtoml-example)), `disable_on_success` adds unattended persistence **and** access to OpenAB's environment, which the agent's shell does not have. For example, `disable_on_success = "env > /tmp/openab-env"` can expose a bot token to the agent.
- If the agent's shell tool is restricted, or the agent runs sandboxed with less privilege than the OpenAB process, but it can still write `cronjob.toml`, then `disable_on_success` grants shell execution the agent would not otherwise have.

> [!CAUTION]
> If your agent reads untrusted content (web pages, RSS feeds, transcripts, messages from other users), a prompt injection that persuades it to add a `[[jobs]]` entry with `disable_on_success` gains recurring shell execution as the OpenAB process user, with OpenAB's credentials and without agent tool approval, and the entry looks like a legitimate schedule. With a bot token from that environment, the command can act as the bot in any channel the bot can reach, including channels that `allowed_users` / `allowed_channels` exclude. The inbound allowlist does not cover this path.

### Choosing the right scope

Prompts and `disable_on_success` live in the same file, so write access cannot be split by field. Choose one of:

- **Full autonomy**: enable usercron and let the agent manage `cronjob.toml`. Best for a personal agent whose inputs you trust.
- **Operator-managed only**: leave `usercron_enabled = false` and put schedules in baseline `[[cron.jobs]]`, or keep usercron but make the file unwritable by the agent at the filesystem level. By default the agent runs as the same OS user as OpenAB, so anything the agent cannot write, OpenAB cannot write either, unless you run them as different users. Options:
  - A read-only mount or a Kubernetes ConfigMap. OpenAB cannot write the file either.
  - A file **and** containing directory owned by a different user than the agent runs as. Write access to the directory alone is enough to replace the file, so making only the file read-only protects nothing. If the directory is shared-writable (such as `/tmp`), also set the sticky bit.

  `usercron_path` may be absolute, so protect the file and directory it actually points to. Agent CLI path rules alone are not enough, because an agent with a shell tool can bypass them.

  OpenAB rewrites the file through a temporary file in the same directory, so scheduler writeback fails whenever OpenAB itself cannot write the file's directory. In that case, do not use `disable_on_success`: an achieved goal cannot be written back as `enabled = false`, so the command keeps running and `✅ Goal achieved` is posted on every schedule match. Also avoid jobs that set `id` but leave `thread_id` unset: the new thread's ID cannot be saved, so a new thread is created on every run. Instead, set a real `thread_id`, or omit `id` if you want a new thread per run.

For an audit trail, record changes somewhere the agent cannot write: push commits to a protected remote, or persist file-monitor events (e.g. from `inotifywait`) to an external or append-only store. A local git repository is not enough, because an agent with a shell tool can rewrite its history. This is detective, not preventive: changes are hot-reloaded and take effect within about a minute.

## Behaviors

- **Minute-aligned**: The scheduler aligns to minute boundaries (`:00`), so `0 9 * * *` fires at exactly 09:00:00, not at whatever second the process started.
- **Overlap protection**: If a previous execution of the same job is still running, the next tick is skipped.
- **Isolation**: Cron failures are logged but never block interactive chat traffic.
- **Usercron persistence**: For usercron jobs, the scheduler may write `thread_id` and `enabled = false` back to `cronjob.toml`.
- **Graceful shutdown**: In-flight cron tasks are waited on (up to 30 seconds) during shutdown.

## Sender Identity

When a cron job fires, the agent sees a sender context like:

```
🕐 [DailyOps]: summarize yesterday's merged PRs
```

Use `sender_name` to distinguish different scheduled tasks in logs and thread titles. The agent can use this to tailor its response (e.g. "DailyOps asked for a summary" vs "WeeklyReport asked for a report").

## Platform Prerequisites

| Platform | Feature Flag | Config / Env Required |
|----------|-------------|----------------------|
| `discord` | (always enabled) | `[discord]` section in config.toml |
| `slack` | `--features slack` | `[slack]` section in config.toml |
| `telegram` | `--features telegram` | `[telegram]` section in config.toml **or** `TELEGRAM_BOT_TOKEN` env var |
| `googlechat` | `--features googlechat` | `[googlechat] enabled = true` in config.toml **or** `GOOGLE_CHAT_ENABLED=true` env var, plus credentials (`sa_key_json`/`sa_key_file`/`access_token` fields or their `GOOGLE_CHAT_*` env equivalents) |
| `lineworks` | `--features lineworks` | `[lineworks]` section in config.toml (or the `LINEWORKS_*` env equivalents) |

> **Note:** The `channel` field for Telegram should be the numeric chat ID (e.g. `"176096071"`). Use [@userinfobot](https://t.me/userinfobot) or the Telegram Bot API `getUpdates` to find your chat ID.

For Google Chat, use the space resource name (for example, `"spaces/AAAA1234567"`). Jobs without `thread_id` stay at the top level of the space because Google Chat does not implement OpenAB's `create_topic` command. To post into an existing thread, set `thread_id` to its full Google Chat thread resource name.

For LINE WORKS, use a channel ID for group talks or `user:<userId>` (or `user:<loginId>`) for 1:1 delivery. LINE WORKS has no thread API, so cron messages always deliver to the flat channel; no synthetic thread is created and `thread_id` has no effect.

## When to Use External Schedulers Instead

Config-driven cron covers the 80% use case: "send this message at this time." For advanced needs, use external schedulers:

| Need | Recommendation |
|---|---|
| Simple recurring prompts | ✅ Config-driven cron (this feature) |
| Long-running jobs (>5 min) | K8s CronJob |
| Conditional logic / retries | GitHub Actions or Step Functions |
| Multi-step workflows / DAGs | GitHub Actions or Step Functions |
| Per-execution isolation | K8s CronJob (separate Pod per run) |

See [Kubernetes CronJob Reference Architecture](cronjob_k8s_refarch.md) for the external scheduler approach.

## Known Limitations

| Limitation | Details |
|---|---|
| Mixed numeric/name day-of-week | `1,Mon` or `Mon,3` is not supported and will be rejected. Use either all numeric (`1-5`) or all name-based (`Mon-Fri`) notation. |
| Wrap-around day-of-week ranges | `5-2` (Fri through Tue) is not supported. Use explicit listing instead: `5,6,0,1,2`. |

> **Tip:** Name-based notation (`Mon-Fri`, `Sun`, `Mon,Wed,Fri`) is always available as an alternative to numeric day-of-week values.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Job never fires | Invalid cron expression | Check logs for `invalid cron expression, skipping` |
| Job fires but no reply | Agent error | Check logs for `cron handle_message error` |
| Wrong time | Timezone mismatch | Set `timezone` explicitly (default is UTC) |
| Job skipped | Previous execution still running | Check logs for `skipping cronjob, previous execution still running` |
| Channel not found | Bot not in channel | Invite the bot to the target channel |
| Usercron not reloading | File not saved / wrong path | Check logs for `usercron file changed, reloading` |
| Usercron parse error | Invalid TOML syntax | Check logs for `failed to parse usercron file` |
| Goal job does not auto-disable | Command did not exit `0` or output did not include `disable_on_success_match` | Run the command manually and confirm both conditions |

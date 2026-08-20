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
# teams_tenant_id = "<tenant-id>"            # required only for platform = "teams"
sender_name = "DailyOps"                     # optional, default: "openab-cron"
timezone = "America/New_York"                # optional, default: "UTC"
thread_id = ""                               # optional: post to existing thread; rejected for Teams
```

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `enabled` | | `true` | Set `false` to disable without removing the entry |
| `schedule` | ✅ | — | 5-field POSIX cron expression |
| `channel` | ✅ | — | Platform destination. For Teams, the exact trusted conversation ID (maximum 2,048 bytes). |
| `teams_tenant_id` | Teams only | — | Exact Teams tenant ID (maximum 256 bytes); rejected on non-Teams jobs. |
| `message` | ✅ | — | Message sent to the agent as a prompt |
| `platform` | | `"discord"` | `"discord"`, `"slack"`, `"telegram"`, `"googlechat"`, `"lineworks"`, or `"teams"` |
| `sender_name` | | `"openab-cron"` | Attribution shown in prompt context |
| `timezone` | | `"UTC"` | IANA timezone (e.g. `"America/New_York"`, `"Europe/Berlin"`) |
| `thread_id` | | — | Post into an existing thread where supported. Teams rejects this field. |

### Microsoft Teams operator baseline

Teams scheduled delivery is available only to operator-owned baseline `[[cron.jobs]]`. It uses the explicit, default-off [trusted persistent conversation registry](adr/teams-trusted-persistent-conversation-registry.md):

```toml
[[cron.jobs]]
enabled = true
schedule = "0 9 * * 1-5"
platform = "teams"
channel = "<teams-conversation-id>"
teams_tenant_id = "<tenant-id>"
message = "summarize yesterday's merged work"
sender_name = "DailyOps"
timezone = "Asia/Taipei"
```

Gateway combines the configured app ID, `teams_tenant_id`, fixed Bot Framework channel `msteams`, and `channel` into the complete registry key. Only an exact active, non-expired record can send. The first visible trigger must be `Delivered` with a real activity ID before any ACP session or agent work begins. Teams jobs do not create or rename threads, do not create conversations, and do not use Graph. Missing capability, disconnected Gateway, registry-off state, or missing/expired/disabled/revoked/mismatched records fail closed without retry.

The stored reference and `serviceUrl` remain Gateway-local. The operator config necessarily contains the non-secret tenant ID, conversation ID, and scheduled prompt; protect that file accordingly.

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

Chart v0.10.0 does not render per-field cron values. Put the complete operator configuration in `agents.<name>.configToml` or load a standalone file with `--set-file`:

```yaml
agents:
  kiro:
    configToml: |
      [gateway]
      url = "wss://openab-gateway.example/ws"
      platform = "teams"
      token = "${GATEWAY_WS_TOKEN}"

      [[cron.jobs]]
      schedule = "0 9 * * 1-5"
      platform = "teams"
      channel = "<teams-conversation-id>"
      teams_tenant_id = "<tenant-id>"
      message = "summarize yesterday's merged work"
      sender_name = "DailyOps"
      timezone = "Asia/Taipei"
```

Or preserve TOML syntax highlighting and avoid YAML scalar editing:

```bash
helm upgrade mybot charts/openab \
  --set-file agents.kiro.configToml=./config.toml
```

Do not add a parallel Teams target under legacy `agents.<name>.cronjobs`; the raw Core config is the only operator schedule authority. The registry path and Microsoft credentials remain Gateway-side configuration.

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
>
> ```bash
> mkdir -p ~/.openab
> mv ~/cronjob.toml ~/.openab/cronjob.toml
> ```

### Create `cronjob.toml`

The external file uses `[[jobs]]` and the same non-Teams scheduling fields. Teams is intentionally different: every usercron entry with `platform = "teams"` or `teams_tenant_id` is rejected before route lookup, platform HTTP, session creation, or ACP work.

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

### How Usercron Reloading Works

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

This enables mobile-friendly schedule management for supported non-Teams destinations — talk to your agent from your phone, and it updates the cron file for you.

> [!CAUTION]
> Agent-managed usercron cannot target Teams. `/remind` and other user-created schedules also do not inherit operator cron authority; a future design must bind and revalidate the initiating user and trusted scope.

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
3. If the command exits `0` and output contains `disable_on_success_match`, OpenAB posts `✅ Goal achieved`, writes `enabled = false` back to `$HOME/.openab/cronjob.toml`, and skips the regular prompt.
4. Otherwise, OpenAB sends the regular `message` and the agent continues working.

`disable_on_success` is supported only in usercron `[[jobs]]`, not baseline `[[cron.jobs]]`. This keeps scheduler writeback limited to the user-managed cron file.

### Re-enabling a Disabled Job

Once a goal is achieved and the job is disabled, re-enable it by editing `$HOME/.openab/cronjob.toml`:

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
| `teams` | Standalone Gateway or `--features teams` Unified | A safely opened Gateway conversation registry plus negotiated persistent-send capability; baseline `teams_tenant_id` and `channel` must match one active record. |

> **Note:** The `channel` field for Telegram should be the numeric chat ID (e.g. `"176096071"`). Use [@userinfobot](https://t.me/userinfobot) or the Telegram Bot API `getUpdates` to find your chat ID.

For Google Chat, use the space resource name (for example, `"spaces/AAAA1234567"`). Jobs without `thread_id` stay at the top level of the space because Google Chat does not implement OpenAB's `create_topic` command. To post into an existing thread, set `thread_id` to its full Google Chat thread resource name.

For LINE WORKS, use a channel ID for group talks or `user:<userId>` (or `user:<loginId>`) for 1:1 delivery. LINE WORKS has no thread API, so cron messages always deliver to the flat channel; no synthetic thread is created and `thread_id` has no effect.

For Teams, copy the tenant and conversation IDs from an active record in the
[trusted persistent conversation registry](adr/teams-trusted-persistent-conversation-registry.md).
Do not configure `serviceUrl`; it is retained and validated only by Gateway.
Personal, groupChat, and channel delivery use the stored conversation itself,
and `thread_id` is a startup error.

## When to Use External Schedulers Instead

Config-driven cron covers the 80% use case: "send this message at this time." For advanced needs, use external schedulers:

| Need | Recommendation |
|---|---|
| Simple recurring prompts | ✅ Config-driven cron (this feature) |
| Long-running jobs (>5 min) | K8s CronJob |
| Conditional logic / retries | GitHub Actions or Step Functions |
| Multi-step workflows / DAGs | GitHub Actions or Step Functions |
| Per-execution isolation | K8s CronJob (separate Pod per run) |

See [Kubernetes CronJob Reference Architecture](refarch/cronjob_k8s_refarch.md) for the external scheduler approach.

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
| Teams baseline is rejected at startup | Missing/invalid `teams_tenant_id`, `thread_id` set, or no Teams adapter configured | Correct the baseline target and choose exactly one Standalone or Unified Teams adapter |
| Teams tick is skipped before ACP | Gateway disconnected, old peer, registry unavailable, or exact record not active | Restore the single-consumer Gateway and refresh the record with a newly trusted inbound activity |
| Teams usercron entry is skipped | Agent-writable usercron has no persistent-route authority | Move an operator-approved schedule into baseline `[[cron.jobs]]` and restart |

## Maintaining This Guide

- **Trigger:** cron config schema, scheduler timing, usercron writeback,
  supported platform dispatch, or Teams registry authority changes.
- **Action:** update this guide from `crates/openab-core/src/cron.rs` and run:

  ```bash
  cargo test -p openab-core cron
  helm template test charts/openab \
    --set-file agents.kiro.configToml=config.toml.example
  ```

- **Why:** scheduler validation is authoritative; examples must never grant
  agent-writable schedules more destination authority than the runtime.

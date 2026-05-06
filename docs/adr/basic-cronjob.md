# ADR: Basic CronJob Support (Config-Driven Scheduled Messages)

- **Status:** Proposed
- **Date:** 2026-04-26
- **Author:** @chaodu-agent
- **Related:** [K8s CronJob Reference Architecture](../cronjob_k8s_refarch.md)

---

## 1. User Story & Requirements

As an OpenAB operator, I want to define scheduled messages in `config.toml` that are sent to the agent at specified times, so that I can automate recurring tasks (daily summaries, weekly reports, periodic scans) without external infrastructure.

As a small-team user, I don't want to set up Kubernetes CronJobs or GitHub Actions just to send a daily prompt to my bot.

Requirements:
- Cron expressions defined declaratively in `config.toml`
- Each cron entry specifies a target channel and a message payload
- OAB sends the message to the agent via the existing ACP session, as if a user typed it
- The scheduler is internal to OAB but **does not** allow the agent to configure or modify cron entries at runtime
- No coupling between scheduler failures and gateway stability — failed cron executions must not block or degrade normal chat traffic
- This is Phase 1 only — complex scheduling (conditional logic, retries, fan-out) remains the domain of external schedulers (K8s CronJob, GitHub Actions)

---

## 2. Context & Decision Drivers

### Lessons from In-App CronJob (OpenClaw Pattern)

The "in-app cronjob" pattern — where the agent configures its own cron via natural language — has known issues:

1. **Opacity** — users cannot easily inspect what the agent configured
2. **Reliability** — continuous cron failures or too many in-flight jobs affect gateway stability
3. **Unpredictability** — agent-generated payloads may not match user intent

### Design Principle: Decouple Scheduler from Core

> "If you already have a good landlord doing scheduling, you don't need your own alarm clock in the room."

OAB's position: the scheduler should be **external to the agent runtime**. For Phase 1, "external" means config-driven (operator-controlled, not agent-controlled). For advanced use cases, truly external schedulers (K8s, GHA) are recommended.

### Why Config-Driven First

- Covers the 80% use case: "send this message at this time"
- Zero additional infrastructure for simple deployments
- Operator has full visibility and control over what runs and when
- Aligns with OAB's "keep it simple" philosophy

---

## 3. High-Level Design

### Configuration

```toml
[[cronjobs]]
schedule = "0 9 * * 1-5"                    # cron expression (UTC)
channel = "123456789"                        # target channel ID
message = "summarize yesterday's merged PRs" # message sent to agent
platform = "discord"                         # optional, defaults to first configured
sender_name = "CronScheduler"               # optional, defaults to "openab-cron"
thread_id = "1234567890"                     # optional, post to existing thread

[[cronjobs]]
schedule = "0 0 * * 0"
channel = "123456789"
message = "generate weekly status report and post it here"
platform = "discord"

[[cronjobs]]
schedule = "0 18 * * 1-5"
channel = "C0123456789"
message = "check for any critical alerts in the last 8 hours"
platform = "slack"
sender_name = "OpsBot"
thread_id = "1714000000.000000"              # Slack thread_ts
```

### Sender Identity

When a cron job fires, the message injected into the agent session includes a sender header so the agent knows who/what triggered it:

- **Default:** `openab-cron` (if `sender_name` is not specified)
- **Customizable:** operators can set `sender_name` per cron entry

The agent sees this in the prompt context the same way it sees a Discord/Slack username, e.g.:

```
[openab-cron]: summarize yesterday's merged PRs
```

This helps the agent distinguish scheduled prompts from interactive users, and operators can use meaningful names like `"DailyOps"` or `"WeeklyReport"` for clarity in logs and thread titles.

### Execution Flow

```
┌──────────────────────────────────────────────────────────────┐
│  OAB Process                                                 │
│                                                              │
│  ┌────────────────┐    tick    ┌──────────────────────┐      │
│  │  Cron Scheduler│──────────▶│  Session Pool        │      │
│  │  (internal)    │           │  (existing)          │      │
│  │                │           │                      │      │
│  │  Evaluates     │  create   │  Spawns ACP session  │      │
│  │  expressions   │──session─▶│  for target channel  │      │
│  │  every minute  │           │                      │      │
│  └────────────────┘           └──────────┬───────────┘      │
│                                          │                   │
│                                          ▼                   │
│                               ┌──────────────────────┐      │
│                               │  Agent (kiro-cli)    │      │
│                               │  Receives message    │      │
│                               │  as if user typed it │      │
│                               └──────────┬───────────┘      │
│                                          │                   │
│                                          ▼                   │
│                               ┌──────────────────────┐      │
│                               │  Platform Adapter    │      │
│                               │  Posts reply to      │      │
│                               │  target channel      │      │
│                               └──────────────────────┘      │
└──────────────────────────────────────────────────────────────┘
```

### Key Behaviors

1. **Evaluation** — a lightweight scheduler thread evaluates cron expressions once per minute
2. **Session creation** — when a cron fires, OAB creates (or reuses) a session for the target channel, identical to a user-initiated session
3. **Message injection** — the configured message is injected as if a user sent it in the channel
4. **Reply delivery** — agent response is posted to the target channel via the normal platform adapter
5. **Thread creation** — each cron execution creates a new thread (Discord) or thread reply (Slack), keeping cron outputs organized. If `thread_id` is specified, the reply is posted to that existing thread instead (useful for accumulating recurring results in one place).
6. **Isolation** — cron execution uses the same session pool with standard concurrency limits; a stuck cron job does not starve interactive sessions
7. **Logging** — each cron tick emits structured logs: `DEBUG` for expression evaluation, `INFO` for fired jobs (schedule, channel, message), `WARN`/`ERROR` for failures (session creation failed, channel not found, agent timeout)

---

## 4. Failure Handling

| Scenario | Behavior |
|---|---|
| Agent session fails to start | Log error, skip this tick, retry next schedule |
| Agent times out | Session TTL applies normally; cron does not retry |
| Channel not found / bot not in channel | Log error, skip; do not crash OAB |
| Overlapping executions (previous still running) | Skip if a session for the same cron entry is still active |
| OAB restart | Cron state is stateless — re-evaluated from config on startup, no persistence needed |

Cron failures are **fire-and-forget** — they log but never block the main event loop or degrade interactive chat performance.

---

## 5. Helm Values Mapping

```yaml
# values.yaml
agents:
  kiro:
    cronjobs:
      - schedule: "0 9 * * 1-5"
        channel: "123456789"
        message: "summarize yesterday's merged PRs"
        platform: "discord"
      - schedule: "0 0 * * 0"
        channel: "123456789"
        message: "generate weekly status report"
        platform: "discord"
```

Rendered into `config.toml` via the existing Helm ConfigMap template.

---

## 6. Scope Boundaries

### In Scope (Phase 1)

- Static cron expressions in `config.toml`
- One-way message injection to agent
- Reply posted to configured channel
- Standard 5-field cron syntax (minute, hour, day-of-month, month, day-of-week) — same format as Linux crontab, Kubernetes CronJob, and GitHub Actions
- Timezone support via optional `timezone` field (defaults to UTC)

### Cron Expression Format

The schedule field uses standard POSIX cron syntax, compatible with:

- Linux `crontab -e`
- Kubernetes `CronJob.spec.schedule`
- GitHub Actions `cron:`
- AWS EventBridge `cron()` (slightly different wrapper but same fields)

```
┌───────────── minute (0–59)
│ ┌───────────── hour (0–23)
│ │ ┌───────────── day of month (1–31)
│ │ │ ┌───────────── month (1–12)
│ │ │ │ ┌───────────── day of week (0–7, 0 and 7 = Sunday)
│ │ │ │ │
* * * * *
```

### Explicitly Out of Scope

| Feature | Reason | Alternative |
|---|---|---
| Agent self-configuring cron | Opacity + reliability concerns (OpenClaw lesson) | Operator edits config |
| Conditional execution | Complexity belongs in external schedulers | K8s CronJob / GHA |
| Retry with backoff | Over-engineering for Phase 1 | External scheduler |
| Cron result persistence / history | Adds state management to OAB | External observability |
| Multi-step workflows / DAGs | Not a scheduler's job | GitHub Actions / Step Functions |
| Dynamic cron via API / Discord command | Requires runtime state, auth, validation | Future ADR if needed |

---

## 7. Comparison with External Scheduler Approaches

| Aspect | Config-Driven (this ADR) | K8s CronJob | GitHub Actions |
|---|---|---|---|
| Infrastructure needed | None (built into OAB) | K8s cluster | GitHub repo |
| Isolation | Shared OAB process | Separate Pod per execution | Separate runner |
| Scalability | Bounded by session pool | Unlimited (Pod per job) | Unlimited (runner per job) |
| Complexity | Minimal | Medium (manifests, RBAC) | Low-Medium (workflow YAML) |
| Visibility | Config file + logs | kubectl + Pod logs | Actions UI + logs |
| Best for | Simple recurring prompts | Heavy/long-running jobs | CI/CD-integrated tasks |

**Recommendation:** Use config-driven cron for simple "send message at time X" use cases. Graduate to K8s CronJob or GitHub Actions when you need isolation, retries, conditional logic, or long-running executions.

---

## 8. Rollout Plan

| Phase | Scope | Deliverable |
|---|---|---|
| **Phase 1 (this ADR)** | Config-driven `[[cronjobs]]` in `config.toml` | Scheduler module, Helm values, docs |
| **Phase 2** | Observability | Cron execution metrics, structured logs, optional webhook on failure |
| **Phase 3** | Enhanced expressions | `@daily`, `@weekly` shortcuts, second-precision (optional), jitter |

---

## Consequences

### Positive

- Zero-infrastructure scheduled tasks for simple use cases
- Operator has full control and visibility — no agent-configured surprises
- Stateless design — no persistence, no migration, restart-safe
- Decoupled from gateway event loop — cron failures cannot degrade chat
- Natural upgrade path to external schedulers for complex needs
- Aligns with OAB's simplicity-first philosophy

### Negative

- Cannot be configured via natural language in conversation (by design)
- Requires config change + restart/reload to modify schedules
- Limited to what a single message prompt can express
- No built-in retry — transient failures are silently skipped until next tick

---

## Compliance

1. **No agent self-configuration**: the agent must not be able to create, modify, or delete cron entries. All scheduling is operator-controlled via config.
2. **Isolation from chat traffic**: cron execution must use the standard session pool with concurrency limits. A cron job must never starve interactive sessions.
3. **Stateless scheduler**: no persistent cron state. Schedule evaluation is purely derived from config + current time.
4. **Standard cron syntax**: expressions must follow the POSIX 5-field cron format. Non-standard extensions must be explicitly documented.

---

## Notes

- **Version:** 0.1
- **Changelog:**
  - 0.1 (2026-04-26): Initial proposed version

---

## References

- [K8s CronJob Reference Architecture](../cronjob_k8s_refarch.md) — external scheduler approach for heavy workloads
- [OpenAB Configuration Reference](../config-reference.md) — existing config.toml structure
- [ADR: Custom Gateway](./custom-gateway.md) — gateway architecture context (scheduled triggers via webhook)
- [cron(5) man page](https://man7.org/linux/man-pages/man5/crontab.5.html) — POSIX cron expression syntax

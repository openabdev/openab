# ADR: OpenAB PR Review Loop

**Status:** Amended  
**Date:** 2026-06-18  
**Author:** chaodu-agent

## Context

OpenAB's PR review workflow currently relies on manual triggers — a human @mentions the review bot in Discord to initiate a review. This works well for ad-hoc reviews but does not scale for repositories with frequent PR activity. Maintainers want automated PR reviews that:

1. Trigger automatically when a PR is opened or updated
2. Show review status as a commit status (🟡 pending → ✅/❌ complete)
3. Preserve the full review process in a Discord thread for auditability
4. Post a single aggregated comment on the PR (hiding previous comments)
5. Work with the existing OpenAB agent running on ECS Fargate (long-lived)

The agent should not need to be ephemeral — it stays running and receives review requests like any other Discord message.

## Decision

Use a **GitHub Action → Discord Webhook → OpenAB (ECS)** architecture with GitHub Commit Status API for check status feedback.

### Why Commit Status API (not Check Runs)

Check Runs API requires a GitHub App with `checks:write` permission. Commit Status API works with a standard PAT or fine-grained token (`commit statuses: write`), which the agent already has via `gh` CLI auth. This avoids creating an additional GitHub App solely for status reporting.

### Why Discord Webhook

- Simplest setup — only one secret (webhook URL), no Bot Token management
- Webhook messages posted to a channel will trigger OpenAB's existing message pipeline via @mention
- OpenAB auto-creates a thread for the conversation (existing behavior)

### Configuration Prerequisites

Discord webhook messages are flagged `author.bot == true` at the API level. OpenAB's Discord adapter defaults to `allow_bot_messages: "off"`, which silently drops bot messages. For this automation to work, the deployment **must** configure one of:

1. Set `allow_bot_messages: "mentions"` — allows bot messages that @mention the agent
2. Add the webhook's author ID to `trusted_bot_ids`

These settings are configured in the OpenAB ECS task definition's environment variables or the agent's runtime configuration file.

**Example (ECS environment variable):**
```
DISCORD_ALLOW_BOT_MESSAGES=mentions
```

**Example (config.toml):**
```toml
[discord]
allow_bot_messages = "mentions"
```

Without this, the webhook @mention will be ignored and reviews will never trigger.

## Amendment: Reactive → Scheduled Polling, Plus Event and Manual Triggers (2026-06-18; amended 2026-06-27, 2026-07-16)

The 2026-06-18 amendment (#1145) replaced the reactive trigger with scheduled polling. The same-repo `pull_request` trigger was restored on 2026-06-27 (#1218, #1219), and the `/review` slash command was added on 2026-07-16 (#1412).

### Problem with Reactive Mode

The original reactive design (`pull_request_target` trigger) fires a webhook on every push. While GitHub Actions' `concurrency` group cancels in-flight runs, once a Discord webhook is delivered it cannot be recalled. This causes:

1. Rapid pushes spawn multiple concurrent agent review sessions in Discord
2. Agent callbacks race — stale reviews may overwrite fresh status
3. Wasted compute and API tokens on superseded commits

### Solution: Scheduled Polling, Plus Event and Manual Triggers

Add a `schedule` cron job that polls every 5 minutes, and keep the event-driven trigger for same-repo PRs. The workflow runs on four triggers:

```yaml
on:
  pull_request:
    types: [opened, synchronize, reopened]
  schedule:
    - cron: '*/5 * * * *'
  workflow_dispatch: {}
  issue_comment:
    types: [created]
```

- `pull_request`: runs the poller as soon as a same-repo PR is opened or pushed. Fork PRs are skipped on this path because their `GITHUB_TOKEN` is read-only (see Negative).
- `schedule`: the sweep. It picks up fork PRs, PRs whose event run failed or was cancelled, and stale `pending` statuses.
- `workflow_dispatch`: manual run of the poller.
- `issue_comment`: a maintainer `/review` comment triggers a targeted review of that one PR.

The `pull_request`, `schedule` and `workflow_dispatch` paths all run the same poller over every open PR, and all of them rely on the status gating below to avoid duplicate reviews.

**Polling logic per open PR:**

| HEAD commit status | Action |
|-------------------|--------|
| `pending` by the coordinator (`chaodu-agent`) (< 2 h) | **Skip** — the coordinator owns an active round (it may be waiting on reviewers) |
| `pending` by the coordinator (≥ 2 h) | **Trigger** — coordinator presumed crashed |
| `pending` by anyone else, e.g. `github-actions[bot]` (< 30 min) | **Skip** — webhook just sent, waiting for the coordinator to take ownership |
| `pending` by anyone else (≥ 30 min) | **Trigger** — stale, agent likely missed the webhook |
| `success` | **Skip** — already reviewed this SHA (LGTM) |
| `failure` | **Skip** — already reviewed this SHA (CHANGES REQUESTED); new push = new SHA = auto-triggers |
| `error` / none | **Trigger** — needs review (new SHA or webhook previously failed) |

The stale timeouts handle the case where the agent is down or missed the webhook — the status would otherwise stay `pending` indefinitely, blocking further reviews. The `creator` of the latest status tells the two cases apart: the poller writes `pending` as `github-actions[bot]` when it sends the webhook, and the coordinator writes its own `pending` as `chaodu-agent` (`COORDINATOR_LOGIN` in the workflow) when it starts a round, which marks the round as owned. Only that login gets the 2 h window; a `pending` written by any other login (the poller, a maintainer, another automation) is treated as unowned and uses the 30 min window.

A maintainer `/review` comment re-reviews a `success` or `failure` SHA right away, and also skips a fresh `pending`. For a coordinator-owned `pending`, `/review` uses the 30 min window instead of 2 h, so a maintainer can take over a crashed round after 30 min. To recover sooner, push a new commit (new SHA) or set the `OpenAB PR Review` status on the SHA to `error`; the next poll re-triggers it.

This gives best-effort deduplication per HEAD SHA: the poller starts at most one review per SHA within the stale window. It is not a per-PR single-flight guarantee. A push during an open round makes the new SHA eligible right away, and `/review` runs use a separate concurrency group (see Mitigations), so two rounds on the same PR can still overlap.

### Trade-offs vs Reactive Mode

| Aspect | Reactive (old) | Scheduled + event (new) |
|--------|---------------|-----------------|
| Latency | Immediate on push | Immediate for same-repo pushes; up to 5 min for fork PRs and retries (cron is best-effort; may lag under GH load) |
| Duplicate reviews | Possible (webhook already sent) | Reduced (skip if pending; push bursts and `/review` can still overlap) |
| GitHub Actions minutes | Per-push (many short runs) | One run per 5 min, plus one short run per same-repo push |
| Complexity | Simple trigger | Polling + state check logic |

### Why This Is Acceptable

- Same-repo PRs still start review on push; the 5-minute sweep only adds latency for fork PRs and retries, which is fine for code review (not user-facing)
- Status gating removes most duplicate reviews on its own; agent SHA validation remains as belt-and-suspenders. Rounds can still overlap in the cases listed under the polling table
- `workflow_dispatch` and `/review` allow manual triggers for urgent reviews
- One poller run covers all PRs, so retries and fork PRs do not add per-PR runs
- **Cron caveat:** GitHub Actions `schedule` is best-effort — actual interval may be 5–25 min during platform congestion. This is acceptable since review latency is not user-facing.

### Stale Timeout Rationale (30 minutes / 2 hours)

The 30-min threshold for an unowned `pending` is based on observed review completion times: typical reviews finish in 5–15 minutes (LLM processing + GitHub API calls), and a coordinator that received the webhook writes its own `pending` within a minute. If only the poller's `pending` exists after 30 min, no round is running.

A coordinator-owned round can legitimately take longer than 30 min while it waits for reviewer reports. Re-triggering it at 30 min started a second round on the same SHA (#1621), so owned rounds use a 2-hour hard timeout that only recovers from a crashed coordinator. The coordinator also refuses to start a round when the latest status is already its own `pending`. The cost is that a coordinator crash blocks automatic re-review of that SHA for up to 2 h; the manual recovery paths above cover it.

### Fork PRs & `safe-to-review` in Scheduled Mode

The scheduled poller runs on `schedule` events (base repo context), so it has full access to secrets. The `safe-to-review` label bypass is preserved in the jq filter — PRs with untrusted `author_association` are included if the label is present. Fork PRs are handled the same as before: the poller can set statuses and fire webhooks for any open PR regardless of source, since it operates in the base repo context.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Cron every 5 min / same-repo pull_request / manual / /review        │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ polls open PRs (/review: only that PR, FORCE=1)
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│  GitHub Action (.github/workflows/pr-bot-review.yml)                │
│                                                                     │
│  For each eligible PR (/review: only its own PR, FORCE=1):          │
│  1. Check HEAD commit status for "OpenAB PR Review"                 │
│     - pending by coordinator (< 2 h; /review: < 30 min) → SKIP      │
│     - pending by coordinator (≥ 2 h; /review: ≥ 30 min) → TRIGGER   │
│     - other pending (< 30 min) → SKIP (webhook just sent)           │
│     - other pending (≥ 30 min) → TRIGGER (stale)                    │
│     - success / failure → SKIP (/review: TRIGGER, re-review)        │
│     - error / none → TRIGGER                                        │
│                                                                     │
│  2. POST /repos/{owner}/{repo}/statuses/{sha}                       │
│     state: "pending", context: "OpenAB PR Review"                   │
│                                                                     │
│  3. Discord Webhook: "@bot review <PR_URL>"                         │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ Discord message
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│  OpenAB Agent (ECS Fargate, long-lived)                             │
│                                                                     │
│  Receives @mention → opens agent session (auto-creates thread)      │
│  → Delegates to reviewer team (angle-based review)                  │
│  → Collects findings in Discord thread                              │
│  → Aggregates into single review comment                            │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ review complete
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│  Post Results to GitHub                                             │
│                                                                     │
│  1. Minimize all previous chaodu-agent comments (GraphQL)           │
│  2. Post aggregated review comment (gh pr comment)                  │
│  3. Update commit status:                                           │
│     → "success" if LGTM ✅                                          │
│     → "failure" if CHANGES REQUESTED ⚠️                             │
└─────────────────────────────────────────────────────────────────────┘
```

## Review Loop (Auto-Fix Cycle)

The architecture supports a closed-loop review cycle:

```
                    ┌─────────────────────────────────────────┐
                    │                                         │
                    ▼                                         │
┌──────────┐    ┌─────────────────┐     ┌──────────────┐      │
│  PR push │───▶│  GitHub Action   │───▶│  OpenAB      │      │
│          │    │  (set pending)   │    │  Review      │      │
└──────────┘    └─────────────────┘     └──────┬───────┘      │
                                              │               │
                                    ┌─────────┴─────────┐     │
                                    │                   │     │
                                    ▼                   ▼     │
                             ┌────────────┐    ┌────────────┐ │
                             │  LGTM ✅   │    │ CHANGES    │ │
                             │            │    │ REQUESTED  │ │
                             └─────┬──────┘    └─────┬──────┘ │
                                   │                 │        │
                                   ▼                 ▼        │
                             ┌────────────┐    ┌────────────┐ │
                             │  status:   │    │ Auto-fix   │ │
                             │  success   │    │ commit +   │─┘
                             │  (done)    │    │ push       │
                             └────────────┘    └────────────┘
                                              (re-triggers Action)
```

By default, the agent **only reviews and reports findings** — it does not push fixes automatically. The auto-fix loop is only activated when a human (maintainer) explicitly requests it (e.g. "fix and push" or option 4️⃣ in the post-review menu).

When explicitly requested:

1. Agent fixes the code directly on the PR branch
2. Commits and pushes the fix
3. The new SHA has no status → next poll cycle triggers a new review
4. Repeat until LGTM or max iterations reached

### Safeguards

- **Max iterations** — agent enforces a soft cap (3 cycles per auto-fix request) to prevent runaway fixes within a single session. The workflow's circuit breaker (30 cycles) is a hard cap across the entire PR lifetime, catching edge cases where the agent cap is bypassed (e.g. multiple maintainer requests).
- **Human-only issues** — if findings require design decisions or are ambiguous, the agent requests human input instead of auto-fixing
- **Commit attribution** — auto-fix commits are authored by `chaodu-agent` with a clear prefix (e.g. `fix(review):`) so the loop is auditable

### When Auto-Fix Is Skipped

- Any 🔴 Critical finding (correctness, security) — requires human judgment
- Ambiguous 🟡 findings where multiple valid solutions exist
- Maintainer explicitly opts out of auto-fix for the PR

## Dedup & Performance

The poller deduplicates reviews through commit status, with an agent-side SHA check as a second layer:

### Primary Dedup: Status-Based Gating

The poller checks HEAD commit status before triggering. If status is a fresh `pending` (within the creator-specific window in the polling table above), the PR is skipped, so the poller starts at most one review per SHA within the stale window (best effort; see the note under the polling table). Each same-repo push also starts a poller run. The shared concurrency group cancels an in-flight run when a newer one starts, but a webhook that was already sent cannot be recalled, so a push burst can trigger one review per pushed SHA. The agent-side SHA check below skips the ones that are no longer HEAD.

### Agent-Side SHA Validation (Belt-and-Suspenders)

Even with polling, a narrow race is possible (push arrives between status check and webhook delivery). The agent still validates:

1. Agent extracts `__commit: <SHA>__` from the trigger message
2. Agent queries current PR HEAD: `gh pr view <N> --json headRefOid --jq .headRefOid`
3. If request SHA ≠ HEAD → skip review, respond "Superseded by newer commit"
4. If request SHA = HEAD → proceed with normal review

This prevents wasting API tokens and reviewer compute on commits that are no longer relevant.

### Cost Impact

Without dedup, N rapid pushes could trigger N full reviews (~5 LLM calls each for angle-based delegation). With both layers active, the poller triggers at most one review per SHA, and the agent skips any SHA that is no longer HEAD when it starts, so a push burst usually costs one full review plus a few short "superseded" replies.

## Implementation Plan

### Phase 1: GitHub Action Workflow

> **Canonical source:** [`.github/workflows/pr-bot-review.yml`](../../.github/workflows/pr-bot-review.yml)
>
> Refer to the workflow file for the current implementation. Key design points:

- **Trigger:** `pull_request` (same-repo PRs, opened/synchronize/reopened) + `schedule` (cron `*/5 * * * *`) + `workflow_dispatch` for manual runs + `issue_comment` for maintainer `/review` commands
- **Polling logic:** iterates all open PRs, checks HEAD commit status, skips if `pending` (fresh, per the creator-specific window in the polling table), `success`, or `failure`
- **Guard condition:** skips drafts, untrusted authors, and PRs with `review-limit-reached` label
- **Steps:** poll open PRs → for each eligible PR: circuit breaker check → set pending status → trigger Discord webhook → error fallback on failure

### Phase 2: Agent Callback (Status Update)

After the agent posts the final PR comment, update the commit status (using the comment's `html_url` from the API response as `target_url` so "Details" links directly to the review):

```bash
# LGTM
gh api repos/OWNER/REPO/statuses/SHA \
  -f state="success" \
  -f context="OpenAB PR Review" \
  -f description="LGTM ✅" \
  -f target_url="<comment_html_url>"

# Changes Requested
gh api repos/OWNER/REPO/statuses/SHA \
  -f state="failure" \
  -f context="OpenAB PR Review" \
  -f description="Changes Requested ⚠️" \
  -f target_url="<comment_html_url>"
```

### Phase 3: Branch Protection

Add `OpenAB PR Review` as a required status check in branch protection rules. This enforces that PRs cannot merge until the review completes successfully.

**Setup:** Repository Settings → Branches → Branch protection rules → Edit `main` → Require status checks to pass before merging → Add `OpenAB PR Review` to the required checks list.

## Token & Permissions

| Secret | Purpose | Minimum Permission |
|--------|---------|-------------------|
| `GITHUB_TOKEN` (Actions) | Set pending status + circuit breaker label | `statuses: write` + `issues: write` |
| `OAB_REVIEW_ACTION_WEBHOOK` | Post review request to Discord channel | Webhook URL (channel-scoped) |
| Agent's `gh` auth (PAT) | Post comment + update status + push auto-fix | `repo` (classic) or `contents: write` + `pull_requests: write` + `commit statuses: write` (fine-grained) |

### GitHub Actions Secrets Setup

| Secret Name | Value |
|-------------|-------|
| `OAB_REVIEW_ACTION_WEBHOOK` | Discord channel webhook URL (Settings → Integrations → Webhooks) |
| `OAB_REVIEW_ACTION_BOT_UID` | Discord user ID of the bot to @mention (e.g. the review agent's UID) |

`GITHUB_TOKEN` is automatically provided by Actions — no manual setup needed.

## Consequences

**Positive:**
- Fully automated — no manual @mention needed for PR reviews
- PR Checks tab shows live review status (🟡 → ✅/❌)
- Can enforce review via branch protection rules
- Discord thread preserves full review audit trail (OpenAB auto-creates threads)
- No architectural changes to OpenAB — agent receives messages normally
- Fire-and-forget Action — no runner time wasted waiting for review
- Minimal secrets — only one webhook URL needed in GitHub Secrets

**Negative:**
- Fork PRs and retries wait for the next scheduled poll, up to 5 min (GitHub cron is best-effort, may lag further under load)
- If OpenAB agent is down, status stays "pending" until stale timeout re-triggers (30 min if no coordinator took the round, 2 h if one did)
- Webhook messages lack user identity — OpenAB must allow webhook-originated messages
- Fork PRs: `pull_request` runs triggered by a fork get no secrets and a read-only `GITHUB_TOKEN` (GitHub security policy), so the workflow skips the event-driven path for fork PRs instead of failing. The scheduled poller and the maintainer `/review` command run in the base repository with secrets and a write token, so fork PRs from trusted authors (or labeled `safe-to-review`) are still reviewed by the poller on its next run. A maintainer can also comment `/review` for an immediate review; this checks only the commenter's association, so it bypasses the PR-author gate by design (the maintainer is asserting the diff is safe). Note: the `safe-to-review` label does **not** grant secrets to fork-triggered runs — it only bypasses the poller's `author_association` gate.

**Mitigations:**
- Filter: skip draft PRs and untrusted authors — only `OWNER`, `MEMBER`, `COLLABORATOR`, and `CONTRIBUTOR` (returning contributor with merged PR) trigger automatic review. First-time contributors and unknown authors are skipped; maintainers can add `safe-to-review` label or manually @mention the agent.
- Dedup: status-based gating — `pending` (fresh) and `failure`/`success` skip review. Only new SHAs with no status or stale `pending` (≥ 2 h when written by the coordinator, ≥ 30 min otherwise) trigger.
- Concurrency: scheduled, `pull_request` and `workflow_dispatch` runs share one `concurrency` group, so overlapping poller runs do not race each other. `/review` runs use a separate per-PR group, so a `/review` run and a poller run can still overlap.
- Error fallback: webhook failure marks status as "error", which will be retried on next poll cycle.
- Timeout: a coordinator-written `pending` ≥ 2 h old is treated as a crashed coordinator, and any other `pending` ≥ 30 min old as agent-down; both are re-triggered automatically.

## Safeguards

### Trusted Contributor Filter

The workflow uses GitHub's `author_association` field to gate automatic reviews. Only PRs from trusted authors trigger the review pipeline:

| `author_association` | Auto-review? | Meaning |
|---------------------|--------------|---------|
| `OWNER` | ✅ | Repository owner |
| `MEMBER` | ✅ | Organization member |
| `COLLABORATOR` | ✅ | Explicitly granted write access |
| `CONTRIBUTOR` | ✅ | Has previously merged a PR |
| `FIRST_TIME_CONTRIBUTOR` | ❌ | First PR to this repo |
| `NONE` | ❌ | No prior relationship |

**Why:** Prevents token waste and prompt-injection risk from untrusted PR diffs being fed into agent context. Maintainers can still manually @mention the agent to review skipped PRs after visual inspection.

### Label Override: `safe-to-review`

Maintainers can add the `safe-to-review` label to any PR to bypass the poller's `author_association` check. The workflow has no `labeled` trigger: the next scheduled poll picks the PR up, allowing untrusted contributors' PRs to be reviewed automatically after a maintainer has visually confirmed the PR is safe. A maintainer `/review` comment triggers an immediate review instead of waiting for the poll.

**Important:** This works for **same-repo and fork PRs**. The label does not give fork-triggered `pull_request` runs any secrets (those runs are skipped), but the poller runs in the base repository with secrets, so a labeled fork PR is reviewed on the next poll.

### Auto-Fix Mode: `auto-fix`

When the `auto-fix` label is present, the webhook payload includes `__mode: auto-fix__`. The agent enters an iterative loop:

1. Review PR → identify actionable findings (🔴/🟡)
2. Fix all findings → push commit
3. Re-review until LGTM or max iterations reached (agent-side cap, recommended: 3)

When the auto-fix loop completes (LGTM or cap reached), the agent removes the `auto-fix` label to prevent subsequent pushes from re-entering the fix loop.

**Constraints:**
- Only effective on same-repo branches (agent needs push access)
- Fork PRs: review still runs via the scheduled poller or `/review`, but auto-fix is not supported for fork branches (pushes fail unless the contributor allows edits by maintainers)
- Agent must implement iteration cap to prevent infinite push→review loops

### Circuit Breaker (workflow hard cap: 30)

The workflow enforces a hard cap of 30 review cycles per PR (across all triggers over the PR's lifetime). This is distinct from the agent-side soft cap of 3 cycles per auto-fix session — the workflow cap catches edge cases where multiple auto-fix requests accumulate. On each run, it counts how many `pending` statuses with context `"OpenAB PR Review"` exist across all commits in the PR. If the count reaches 30:

1. Adds `review-limit-reached` label to the PR
2. Sets commit status to `error` with description "Circuit breaker: exceeded 30 review cycles"
3. Fails the workflow step

The `review-limit-reached` label is checked in the job `if` condition — once applied, no further review runs will trigger. A maintainer can remove the label to reset the circuit breaker if needed.

## References

- [GitHub Commit Status API](https://docs.github.com/en/rest/commits/statuses)
- [Discord Webhooks](https://discord.com/developers/docs/resources/webhook#execute-webhook)
- OpenAB PR Review Spec — internal agent document (not in this repository)

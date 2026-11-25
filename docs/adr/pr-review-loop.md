# ADR: OpenAB PR Review Loop

**Status:** Amended  
**Date:** 2026-06-18  
**Author:** chaodu-agent

## Context

Reviews were started by hand: a maintainer @mentioned the review agent in Discord. The goal is to start reviews automatically and show the result as a commit status on the PR head.

An earlier design sent a Discord webhook on every push. Webhooks cannot be recalled, so rapid pushes started reviews of commits that were already superseded. The current design polls open PRs on a schedule and also reacts to a few events.

## Decision

The workflow `.github/workflows/pr-bot-review.yml` starts a review in two steps:

1. Set a `pending` commit status with context `OpenAB PR Review` on the PR head SHA.
2. Post a message to a Discord webhook. The message @mentions the review agent, names the PR, and includes the SHA.

The agent sets the final status (`success` or `failure`) after its review. The workflow does not do that step.

Implemented triggers, declared in the `on:` block (L3-14):

- `pull_request`: `opened`, `synchronize`, `reopened`.
- `schedule`: every 5 minutes (`*/5 * * * *`).
- `workflow_dispatch`: manual run.
- `issue_comment`: `created`. The job runs only for a `/review` comment on a PR, from an OWNER, MEMBER, or COLLABORATOR.

The job-level `if` skips `pull_request` events from forks (L47). A fork run gets a read-only token, so it cannot write statuses.

What a run does:

- A `/review` run polls only its own PR, adds a 👀 reaction, and re-reviews the SHA even if it already has `success` or `failure`.
- Any other run lists all open PRs, skips drafts, and checks the latest `OpenAB PR Review` status on each head SHA:

| Latest status | Action |
|---|---|
| none, or `error` | trigger |
| `success`, `failure` | skip |
| `pending` by `chaodu-agent` (the coordinator) | skip for 2 h, then trigger |
| `pending` by any other login | skip for 30 min, then trigger |

A `/review` run uses 30 min instead of 2 h for a coordinator-owned `pending` (L130).

Circuit breaker: before triggering, the workflow counts `pending` statuses with this context across all commits of the PR. At 30 or more (L181), it adds the `review-limit-reached` label, sets `error` on the SHA, and skips the PR.

Concurrency: each `/review` run has its own group per PR (`pr-review-poller-<number>`, L19). `pull_request`, `schedule`, and `workflow_dispatch` share the group `pr-review-poller-poll` with `cancel-in-progress: true` (L20), so a newer run cancels an older one.

## Consequences

**Latency.** A same-repo PR that opens, gets a push, or reopens starts a run at once. Other changes wait for the next scheduled run, at most about 5 minutes by the cron interval. GitHub may delay scheduled runs; this ADR does not measure that delay. The following changes wait for the next scheduled run:

- Fork PRs. The poll reviews them, but no event run starts.
- A draft marked ready for review. `ready_for_review` is not in the trigger types (L5).
- Label changes. `labeled` is not in the trigger types (L5).

**Run count.** One run per 5-minute tick, plus one run per same-repo `pull_request` event, plus one run per `/review` comment. A draft PR that opens starts a same-repo run that finds nothing to review. Every run except `/review` lists all open PRs (L85).

**Dedup limits.**

- Dedup is per SHA and best-effort. The poll step reads the status and the trigger step writes `pending` (L194) later in the same job. Two runs can both read "no status" before either writes, so both can trigger the same SHA.
- Each `/review` restarts a finished SHA, so a maintainer can start repeated reviews of the same commit.
- A `/review` can take over a coordinator-owned `pending` after 30 min. That can start a second round while the coordinator still works on the first. The workflow comment near the 2 h rule records that a 30 min re-trigger caused a duplicate round (#1621).
- The circuit breaker counts every `pending` status on the PR. This workflow never deletes statuses, so the count never falls. Removing `review-limit-reached` does not reset it, and the next run re-applies the label.
- A coordinator-owned `pending` blocks automatic re-review of that SHA for up to 2 h when the coordinator has crashed. A new push (new SHA) or a manual `error` status on the SHA lets the next poll trigger.

**Draft handling.** Draft PRs are excluded from the poll list and from `/review`. A draft receives a review only after it is marked ready and the next scheduled run (or a manual `workflow_dispatch`) finds it.

## Open questions

1. **Should the `pull_request` trigger stay?** The types list at L5 starts a run on every push to a same-repo PR. The reactive design was rejected for this reason. Options: keep the trigger (one extra webhook per push), remove it (poll-level latency), or add a debounce.
2. **Should an event run scan every open PR?** Every run except `/review` lists all open PRs (L85), so a `pull_request` event on one PR can start reviews on other PRs. Is that intended? Or should the event path target its own PR number, as `/review` does?
3. **Should `/review` take over a coordinator-owned `pending` after 30 min?** The poll path waits 2 h (`OWNED_STALE_SECONDS`, L61). `/review` uses 30 min (L130). Options: keep the 30 min takeover, use 2 h for `/review` too, or require a second confirmation.
4. **Does `cancel-in-progress` on the shared poll group drop a review?** Untested: the shared `pr-review-poller-poll` group uses `cancel-in-progress: true` (origin/main:.github/workflows/pr-bot-review.yml lines 19-20). A newer poll or `pull_request` run may cancel an older one after the older run has written `pending` and before it has sent its webhook. This would leave a SHA marked `pending` with no review. This has not been reproduced; it is a design question, not a confirmed defect.

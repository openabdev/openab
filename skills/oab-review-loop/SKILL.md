---
name: oab-review-loop
description: Handle automated PR review requests triggered by the GitHub Action polling loop. Parse the webhook message, validate SHA, perform review, post results, and update commit status.
---

# oab-review-loop — Automated PR Review Loop

## Trigger

When a message arrives from the OAB Review Actions Hook bot (sender_id: `1516925202951442575`) containing:
- `review <PR_URL>`
- `__commit: <SHA>__`
- Optionally `__mode: auto-fix__`

## Workflow

### 1. Validate SHA (Dedup)

```bash
gh pr view <N> --repo openabdev/openab --json headRefOid -q .headRefOid
```

- If request SHA ≠ current HEAD → respond "Superseded by newer commit `<HEAD>`" and skip.
- If request SHA = HEAD → proceed.

### 2. Baseline Check (Step 0)

Before reading the diff:
1. `gh pr view <N>` — note title, open date, labels, author
2. Check main for existing relevant code
3. Compare: what does the PR add that main doesn't already have?

### 3. Perform Review

Follow the standard PR review spec (`~/.openab/memory/shared/pr-review-spec.md`):
- Read the diff thoroughly
- Evaluate correctness, architecture, docs/UX, security/CI, spec alignment
- Check CI status (`gh pr checks`)
- Classify findings: 🔴 Critical, 🟡 Important, 🟢 Praise

### 4. Post Results to GitHub

1. **Minimize all previous chaodu-agent comments** (mark OUTDATED via GraphQL `minimizeComment`)
2. **Post ONE consolidated comment** (`gh pr comment`) following the review format spec
3. **Update commit status:**

```bash
# Capture the comment URL from gh pr comment output
COMMENT_URL="<html_url from step 2>"

# LGTM
gh api repos/openabdev/openab/statuses/<SHA> \
  -f state="success" \
  -f context="OpenAB PR Review" \
  -f description="LGTM ✅" \
  -f target_url="$COMMENT_URL"

# Changes Requested
gh api repos/openabdev/openab/statuses/<SHA> \
  -f state="failure" \
  -f context="OpenAB PR Review" \
  -f description="Changes Requested ⚠️" \
  -f target_url="$COMMENT_URL"
```

### 5. Discord Response

Reply in the thread with:
- Verdict summary (中文)
- Key findings
- Post-review options for 主人

### 6. Post-Review Options

Always offer:
```
1️⃣ Approve PR
2️⃣ 請 contributor 修改後再 review
3️⃣ 關閉 PR
4️⃣ 我自己來 fix，push 後讓法師團隊 review 直到完全修正
```

## Auto-Fix Mode

When `__mode: auto-fix__` is present in the trigger message:

1. Review → identify actionable findings (🔴/🟡)
2. Fix all findings → push commit with prefix `fix(review):`
3. Do NOT re-trigger self — the next poll cycle will pick up the new SHA
4. **Cap: 3 iterations per auto-fix session** — after 3 cycles, stop and request human input
5. Skip auto-fix for:
   - 🔴 Critical findings requiring design decisions
   - Ambiguous 🟡 findings with multiple valid solutions
6. On completion (LGTM or cap), remove `auto-fix` label: `gh pr edit <N> --remove-label auto-fix`

## Rules

- Only ONE visible comment per PR on GitHub — minimize all previous ones first
- English only for GitHub comments; 中文 flexible in Discord
- Never expose internal 法師 names in GitHub comments
- Always include `target_url` pointing to the review comment when setting commit status
- Status context must be exactly `"OpenAB PR Review"` (matches branch protection)
- If `gh auth status` fails, use device flow to re-authenticate before posting

## Error Handling

If review cannot complete (e.g. auth failure, API error):

```bash
gh api repos/openabdev/openab/statuses/<SHA> \
  -f state="error" \
  -f context="OpenAB PR Review" \
  -f description="Review failed: <reason>"
```

This ensures the next poll cycle (stale timeout 30 min) will re-trigger the review.

---
name: pr
description: Pull request workflow — request peer reviews, address feedback, and iterate until LGTM.
---

# pr — Pull Request Workflow

## Trigger

After creating a pull request.

## Workflow

1. **Submit PR** — Create the PR as usual (via `gh pr create` or other means)
2. **Request reviews** — Immediately ask peers to review by mentioning 一群法師 as role alias.
   Ask the user who to add if not specified.
   When requesting review, remind reviewers to:
   - **Mention 超渡法師 explicitly** (`<@1490365068863606784>`) in their feedback comments
   - **Use reply_to** (threaded replies) so 超渡法師 gets notified
   This ensures no feedback is missed.
3. **Address feedback** — When reviewers leave comments:
   - Read and understand each concern
   - Fix the code accordingly
   - Push the changes
   - Reply to the review comments confirming the fix
4. **Repeat** until all reviewers have approved (LGTM)
5. **Merge** only after all requested reviewers have approved

## Rules

- Never merge without at least one peer review
- Every review comment must be addressed (fixed or discussed) before merging
- Push fixes as new commits (don't force-push over review history unless asked)

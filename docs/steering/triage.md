# Issue Triage Guide for openabdev/openab

## Steps

1. **Confirm type** — ensure one of: `bug`, `feature`, `guidance`
2. **Verify claims** — be skeptical; find source code or official docs to confirm before accepting a bug report as valid
3. **Set priority** — add exactly one:
   - `p0` 🔴 Critical — drop everything
   - `p1` 🟠 High — address this sprint
   - `p2` 🟡 Medium — planned work
   - `p3` 🟤 Low — nice to have
4. **Remove `needs-triage`** — triage complete

## Priority Guidelines

| Priority | Criteria |
|----------|----------|
| p0 | Security vulnerability, data loss, entire system down |
| p1 | Major feature broken for a class of users (e.g. all Claude Code / Cursor users) |
| p2 | Bug with workaround, or planned feature work |
| p3 | Minor improvement, cosmetic, nice to have |

## Response Template

- **Issue at a Glance** — always include an ASCII diagram showing the flow and where things break
- Acknowledge the issue by investigating the relevant source code or official docs
- Confirm root cause or ask clarifying questions
- Link relevant spec/doc references when available
- Invite PR or state next steps
- **Draft response for human approval before posting to the issue comment**

## Issue at a Glance Example

```
Discord User ──► openab ──► Claude Code / Cursor agent
                   │
                   ▼
          session/request_permission
          (agent asks: "can I run this tool?")
                   │
                   ▼
          openab auto-reply (WRONG shape):
          ┌─────────────────────────────────┐
          │ { "optionId": "allow_always" }  │  ← flat, no wrapper
          └─────────────────────────────────┘
                   │
                   ▼
          SDK cannot find `outcome` field
          → treats as REFUSAL ❌
```

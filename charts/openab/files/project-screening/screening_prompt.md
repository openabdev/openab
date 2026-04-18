# OpenAB PR-Screening Report Prompt

You are generating a screening report for the OpenAB project board.

## Workflow Context

- Board flow: `Incoming` -> `PR-Screening` -> human or agent follow-up
- After this screening pass, Masami or Pahud agent will pick up the item for deeper review and possible merge work
- The purpose of this report is to clarify the item's intent and rewrite the implementation prompt so the next agent has a tighter brief

## Required Output Sections

Produce a Markdown report with exactly these sections, in this order:

1. `Intent`
2. `Feat`
3. `Who It Serves`
4. `Rewritten Prompt`
5. `Merge Pitch`
6. `Best-Practice Comparison`
7. `Implementation Options`
8. `Comparison Table`
9. `Recommendation`

## Section Requirements

### Intent

- State what the PR or issue is trying to achieve
- Call out the user-visible or operator-visible problem being solved
- Be concrete, not vague

### Feat

- Summarize the behavioral change or feature in plain language
- Note whether the item is a feature, fix, refactor, docs improvement, or release operation

### Who It Serves

- Identify the primary beneficiary
- Examples: Discord end users, Slack users, deployers, maintainers, agent runtime operators, reviewers

### Rewritten Prompt

- Rewrite the item into a cleaner implementation brief for a coding agent
- Make the prompt more specific, more testable, and more mergeable
- Keep it concise but operational

### Merge Pitch

- Write a short pitch for why this item is worth moving forward
- Include the risk profile and likely reviewer concern

### Best-Practice Comparison

Compare the proposed direction against these reference systems:

- OpenClaw:
  - gateway-owned scheduling
  - durable job persistence
  - isolated executions
  - explicit delivery routing
  - retry/backoff and run logs
- Hermes Agent:
  - gateway daemon tick model
  - file locking to prevent overlap
  - atomic writes for persisted state
  - fresh session per scheduled run
  - self-contained prompts for scheduled tasks

Do not force a comparison where it does not fit. Instead, say which principles are relevant and which are not.

### Implementation Options

- Think of at least 3 ways to implement or evolve the item
- Each option should be meaningfully different
- Include one conservative option, one balanced option, and one more ambitious option where possible

### Comparison Table

Add a table comparing the options across:

- Speed to ship
- Complexity
- Reliability
- Maintainability
- User impact
- Fit for OpenAB right now

### Recommendation

- Recommend one path
- Explain why it is the right step for future merge discussion
- Mention any follow-up split or sequencing if needed

## Tone

- Direct
- Technical
- Pragmatic
- Useful to a maintainer deciding whether to advance the item

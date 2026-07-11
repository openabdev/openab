---
name: reflect
description: Self-reflect on recent conversations to improve existing skills or propose new ones. Use when asked to "reflect", "improve skills", "skill review", or "what did we learn".
---

## Trigger

Invoke with: `reflect`, `skill reflect`, `improve skills`, `what did we learn`

## Workflow

1. **Load recent session history** — Read `~/.kiro/sessions/cli/*.history` files, sorted by modification time (newest first). Scan the top 5–10 sessions to understand recent work patterns. Each `.history` file contains the user's prompts from that session.
2. **Scan existing skills** — Read `~/.kiro/skills/*/SKILL.md` to understand current capabilities
3. **Identify gaps and improvements** — Compare what was done in recent sessions against existing skills:
   - Were there repeated manual steps that a skill could automate?
   - Did an existing skill fail to trigger or miss a workflow step?
   - Was domain knowledge used that isn't captured in any skill?
   - Were new tools/commands used that deserve a skill?
   - Were there multi-step workflows done more than once across sessions?
4. **Output recommendations** — Produce a concise report

## Session History Location

```
~/.kiro/sessions/cli/
├── <uuid>.history   ← user prompts (small, read these)
├── <uuid>.json      ← session metadata
├── <uuid>.jsonl     ← full conversation log (large, skip unless needed)
└── <uuid>.lock      ← active session indicator
```

To find recent sessions, sort `.history` files by mtime and read the newest 5–10.

## Output Format

```markdown
## Skill Reflection

### Improvements to Existing Skills
- **skill-name**: what to add/change and why

### New Skill Proposals
- **proposed-name**: trigger phrase, what it would do, why it's useful

### No Action Needed
- (if nothing actionable, say so briefly)
```

## Rules

- Be concise — bullet points only, no filler
- Only propose skills that would save time on tasks done more than once
- When proposing a new skill, include: name, trigger, and a 1-sentence workflow summary
- When improving an existing skill, reference the specific SKILL.md file path
- If the user confirms a proposal, create/update the skill immediately using the skill-creator guidelines

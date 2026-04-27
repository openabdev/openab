# Feature Request Guide for openabdev/openab

## Purpose

This document guides AI agents (and humans) in crafting well-structured
feature requests for the openabdev/openab project.

**Usage — ask your AI:**

> Per https://github.com/openabdev/openab/blob/main/docs/steering/feature-request.md,
> submit a feature request for me about our previous discussion.

The AI will synthesize the conversation into a feature request and submit it.

## Required Sections

Every feature request MUST include these headings with non-empty content:

### 1. Description

A clear, concise summary of the feature. Answer:
- **What** — What capability is being requested?
- **Where** — Which component/area does it affect? (e.g. discord, slack, session, helm)

Keep it to 2–4 sentences. Lead with the outcome, not the implementation.

### 2. Use Case

Explain **why** this feature is needed. Answer:
- What problem does the user face today?
- What workflow or scenario triggers the need?
- Who benefits? (end users, operators, contributors)

Use concrete examples. "As a server admin, I want X so that Y" is better than
"it would be nice to have X."

### 3. Proposed Solution (optional but encouraged)

If the requester has ideas on implementation:
- Describe the approach at a high level
- Reference relevant code paths or components
- Note any constraints or compatibility concerns
- Use ASCII diagrams for architecture/flow if helpful

## Submission Format

```bash
gh issue create --repo openabdev/openab \
  --title "feat(<scope>): <summary>" \
  --label "feature,<scope-label>" \
  --body '### Description

<description>

### Use Case

<use case>

### Proposed Solution

<proposed solution or remove section if none>'
```

## Title Convention

Format: `feat(<scope>): <short summary>`

- Scope must be one of: `discord`, `slack`, `session`, `helm`, `ci`, `docs`
- Summary should be imperative mood, lowercase, no period (e.g. `add thread pinning support`)
- Keep under 72 characters total

## Quality Checklist

Before submitting, verify:

- [ ] **Title** follows `feat(<scope>): summary` convention
- [ ] **Description** clearly states what the feature is
- [ ] **Use Case** explains why it's needed with a concrete scenario
- [ ] **No duplicates** — search existing issues first: `gh search issues "<keywords>" --repo openabdev/openab --label feature`
- [ ] **Scope label** is included alongside `feature`
- [ ] **References** — link related issues with `#number` if applicable

## AI Agent Instructions

When an AI agent is asked to submit a feature request referencing this doc:

1. **Synthesize** — Distill the conversation into the required sections. Don't copy-paste raw chat; rewrite into clear, structured prose.
2. **Infer scope** — Determine the correct scope from the discussion context.
3. **Check duplicates** — Search for existing issues before creating.
4. **Draft and confirm** — Show the user the formatted issue (title + body) and ask for confirmation before submitting.
5. **Submit** — Use `gh issue create` with the correct labels and format.
6. **Report** — Share the issue URL after creation.

## Example

```
Title: feat(discord): support per-channel session timeout override

Labels: feature, discord

### Description

Allow server admins to configure different session timeout values
per Discord channel, overriding the global default.

### Use Case

In large servers, some channels are used for quick Q&A (short timeout
preferred) while others host long-running collaborative sessions
(longer timeout needed). Currently the global timeout applies
everywhere, forcing admins to pick a compromise value.

### Proposed Solution

Add an optional `session_timeout` field to the per-channel config in
`openab.yaml`. When set, it overrides the global `session.timeout`.
Fall back to global if unset.
```

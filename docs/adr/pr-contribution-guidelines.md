# ADR: PR Contribution Guidelines

- **Status:** Accepted
- **Date:** 2026-04-13
- **Author:** @chaodu-agent

---

## 1. Context & Problem Statement

OpenAB is growing and accepting external contributions. Without a clear PR standard, we see PRs that:

- Jump straight to implementation without explaining the problem
- Don't research how existing projects solve the same problem
- Don't justify why a particular approach was chosen over alternatives
- Make review harder because reviewers must do the research themselves

This front-loads research cost onto the contributor (who understands the problem best) rather than distributing it across reviewers.

**Good example:** Issue #224 / PR #225 (voice message STT) included a thorough prior art investigation — comparing OpenClaw's `audio-transcription-runner.ts` preflight pipeline with Hermes Agent's `transcription_tools.py` local-first approach, producing a clear comparison table, and explaining why OpenAB chose a simpler OpenAI-compatible endpoint design.

## 2. Decision

Establish a standard PR template and tiered contribution guidelines. All PRs document the problem, approach, tradeoffs, and alternatives. Architectural and runtime changes additionally require prior art research (at minimum OpenClaw and Hermes Agent). Docs-only, chore, CI, release, and trivial bug fixes use a lighter process.

### Required PR Sections

Every PR must include these sections in its description:

| # | Section | Purpose |
|---|---------|---------|
| 0 | **Discord Discussion URL** | Strongly recommended. Link to the prior Discord discussion. If none exists, explain context in the PR description. |
| 1 | **What problem does this solve?** | Pain point or requirement in plain language. Link the related issue. |
| 2 | **At a Glance** | ASCII diagram showing the high-level flow, architecture, or where the change fits in the system. "N/A" for docs-only or trivial changes. |
| 3 | **Prior Art & Industry Research** | Required for architectural/runtime changes. "Not applicable" with reason for docs/chore/CI/release/trivial fixes. |
| 4 | **Proposed Solution** | Technical approach, architecture decisions, key implementation details. |
| 5 | **Why This Approach** | Why this over the alternatives from research. Tradeoffs and limitations. |
| 6 | **Alternatives Considered** | Approaches evaluated but not chosen, and why. |
| 7 | **Validation** | Relevant checks for the PR type: Rust, Helm, CI, or docs validation as applicable. |

### Tiered Prior Art Research

Prior art research is **required** for PRs that touch architectural, runtime, agent, scheduling, delivery, or persistence concerns. Contributors must research at minimum these two projects:

| Project | Why it's a reference |
|---|---|
| [OpenClaw](https://github.com/openclaw/openclaw) | Largest open-source AI agent gateway. Plugin architecture across 7+ messaging platforms. Mature patterns for media, security, session management. |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Nous Research's self-hosted agent. Gateway architecture across 17+ platforms. Strong prior art on messaging, tool integration, and service management. |

For each project, document:
- How they solve the same problem (with links to source code or docs)
- Key architectural decisions they made
- What we can learn from their approach

If neither project addresses the problem, state that explicitly with evidence.

For **docs-only, chore, CI, release, or trivial bug fixes**, write "Not applicable — [reason]" in the prior art section.

### Research Flow

```text
Contributor researches prior art
        │
        ▼
┌───────────────────────┐     ┌──────────────────────────────────┐
│ Finds better pattern  │────►│ Adopts it (we benefit)           │
└───────────┬───────────┘     └──────────────────────────────────┘
            │
            ▼
┌───────────────────────────┐ ┌──────────────────────────────────┐
│ Finds different pattern   │►│ Documents why we diverge         │
└───────────┬───────────────┘ │ (reviewers understand tradeoff)  │
            │                 └──────────────────────────────────┘
            ▼
┌───────────────────────────┐ ┌──────────────────────────────────┐
│ Finds nothing relevant    │►│ States so explicitly             │
└───────────────────────────┘ │ (saves reviewers from searching) │
                              └──────────────────────────────────┘
```

## 3. Implementation

| Phase | Deliverable | Description |
|-------|-------------|-------------|
| **1** | `.github/pull_request_template.md` | Auto-populated PR form with all required sections |
| **2** | `CONTRIBUTING.md` | Contributor guide explaining the tiered guidelines and linking to this ADR |
| **3** | Review process update | Reviewers check for prior art section completeness |

## 4. Alternatives Considered

### Option 1: No formal template — rely on reviewer feedback

- Pros: zero contributor friction
- Cons: inconsistent quality, reviewers repeat the same feedback, research burden falls on reviewers

### Option 2: Strict mandatory template everywhere

- Pros: uniform quality across all PRs
- Cons: excessive overhead for trivial/docs/small fixes; may discourage contributions

### Option 3: Tiered policy by PR type (adopted)

- Pros: full prior-art analysis for architectural changes, lighter structure for minor PRs
- Cons: requires defining the boundary between "minor" and "architectural"

## 5. Open Questions

1. Should we enforce the prior art section via CI (e.g., a bot that checks for the section headers)?
2. Should we maintain a living doc of "how OpenClaw/Hermes do X" to reduce per-PR research burden?
3. Are there other mandatory reference projects beyond OpenClaw and Hermes?

## Consequences

- **Positive:** Higher-quality PRs, faster reviews, architectural consistency, less reinventing the wheel
- **Negative:** Higher upfront cost for contributors on architectural PRs; may slow down first-time contributions
- **Mitigation:** Clear examples (PR #225), PR template auto-populates sections, tiered policy keeps docs/chore/CI contributions lightweight

## References

- Issue #224 / PR #225 — exemplary prior art research (STT: OpenClaw vs Hermes Agent comparison)
- [OpenClaw Channel & Messaging Deep Dive](https://avasdream.com/blog/openclaw-channels-messaging-deep-dive)
- [Hermes Agent Messaging Gateway](https://hermes-agent.nousresearch.com/docs/user-guide/messaging/)

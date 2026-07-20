# Steering Design Guide

## Problem

AI coding agents load persistent instructions every session, but without deliberate organization:
- **Bloated instructions** dilute attention — critical rules get buried in noise
- **Duplicated rules** across files inevitably contradict each other when one is updated
- **Missing triggers** mean mandatory behaviors live in docs the agent never reads
- **No shared standard** across agents leads to each team reinventing the wheel

This guide establishes a universal framework for organizing agent memory into layers, so rules are reliably followed, context budgets are respected, and teams can onboard new agents without starting from scratch.

OpenAB is designed to be agent-agnostic — it supports Kiro, Claude Code, Codex, Gemini, Kimi Code, Copilot, OpenCode, and Pi running side by side. This guide provides a shared memory architecture standard that allows all supported coding agents to maintain consistent behavior, collaborate effectively, and operate from a single source of truth regardless of their underlying platform differences.

---

How to organize AI agent memory across three tiers: hot (always loaded), warm (triggered on demand), and cold (searched when needed).

Applies to: Kiro, Claude Code, Codex, Gemini, Kimi Code, Copilot, OpenCode, Pi — any agent that supports persistent instruction files.

---

## Terminology

| Term | Meaning | Examples |
|------|---------|---------|
| 🔥 **Hot memory** | Loaded every session, always in context | `AGENTS.md`, `.kiro/steering/`, `CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md` |
| ☕ **Warm context** | Not always loaded, but auto-triggered when conditions match | Codex Skills (body), Copilot path-specific instructions, CC/Gemini individual memory files (pointed to by hot index), subdir instruction files |
| ❄️ **Cold storage** | Searched or retrieved on demand, no automatic trigger | Knowledge bases, `docs/`, project wikis, ADRs, historical records |

---

## What Goes in Hot Memory

| Criteria | Example |
|----------|---------|
| Every interaction might trigger it | Output format spec, verdict logic |
| Identity & relationships | Agent name, team members, contact IDs |
| SOP trigger words | "review PRs" → auto-execute workflow |
| Hard rules that are easy to get wrong | "NITs are blocking", "never merge", "English only on GitHub" |
| Tool usage patterns | Login flows, API call patterns |
| Constraints that override defaults | "Don't ask for confirmation on X", "Always do Y before Z" |

## What Stays in Cold Storage

| Criteria | Example |
|----------|---------|
| Historical records / case studies | Past incident lessons, collaboration logs |
| One-time reference | Installation steps, migration guides |
| Large data | User profiles, conversation history |
| Design proposals / RFCs | Architecture decisions, feature specs |
| Lookup tables | Feature flags, config reference, changelogs |

## What Goes in Warm Context

| Criteria | Example |
|----------|---------|
| Too large for hot, but has a reliable trigger | Deployment SOP, release checklist |
| Only relevant for specific file types or paths | Gateway adapter checklist, Helm wiring rules |
| Domain-specific expert knowledge | Platform auth spec details, crypto implementation patterns |
| Complex workflows with steps and scripts | Incident triage playbook, skill bodies |

**Rule of thumb:** If it has a clear trigger condition and is > 1KB, make it warm. Keep only the trigger (name + one-line description + path) in hot.

---

## Design Principles

1. **Small and precise** — Keep hot memory concise. Practical caps vary by agent (CC: ~200 lines for MEMORY.md, Codex: 32KB, Kiro: ~15KB recommended). Regardless of hard limits, attention dilution is the real constraint — less is more.
2. **Behavior-oriented** — Every line should change "what the agent does next." Remove anything that's just "nice to know."
3. **Single source of truth** — Define each rule in exactly one place. Duplication across files creates contradiction risk.
4. **Testable** — Each rule should be verifiable with a single prompt from a fresh session.
5. **One file per responsibility** — Separate concerns: identity, review process, workflow triggers. Avoid monolithic instruction files.
6. **Hot/cold separation** — If the agent can find it via search when needed, it doesn't need to be always-loaded.
7. **Structure over prose** — Use lists, tables, or key-value pairs in hot memory. LLMs follow structured constraints more reliably than natural language paragraphs.
8. **WHAT and HOW only** — Hot memory defines what to do and how. Put the WHY (historical context, incident backstory) in cold storage (ADRs, lessons learned).

---

## Decision Flowchart

```
"If this rule is NOT loaded, will the next response be wrong?"
│
├─ Yes → 🔥 Hot memory
│
├─ Only when doing a specific task/touching specific files
│  → ☕ Warm context (put trigger in hot, body in warm)
│
└─ No, it's reference → ❄️ Cold storage
```

---

## Architecture Pattern

```
┌─────────────────────────────────────────────────────────────────┐
│                    🔥 HOT — Always Loaded                        │
│                                                                 │
│  Identity, hard rules, collaboration protocol, trigger index    │
│  AGENTS.md / CLAUDE.md / GEMINI.md / .kiro/steering/*           │
│  .github/copilot-instructions.md / MEMORY.md index              │
│                         < 15KB                                  │
├─────────────────────────────────────────────────────────────────┤
│                 ☕ WARM — Progressive Exposure                    │
│                                                                 │
│  Auto-loaded when trigger condition matches                     │
│  Skills (SKILL.md body), path-specific instructions,            │
│  subdir instruction files, individual memory files,             │
│  domain SOPs, deployment playbooks                              │
│                                                                 │
│  Triggers:  Rule-based (applyTo glob)                           │
│             Semantic (agent reads index, decides to load)        │
│             Explicit (activate_skill / read_file)               │
├─────────────────────────────────────────────────────────────────┤
│                 ❄️ COLD — Search on Demand                       │
│                                                                 │
│  No automatic trigger; requires explicit search/retrieval       │
│  Knowledge bases, docs/*.md, wikis, ADRs, RFCs,                 │
│  historical records, lessons learned, team trivia               │
│                       Unlimited                                  │
└─────────────────────────────────────────────────────────────────┘

         ▲ Smaller, precise, behavioral
         │
         ▼ Larger, reference, historical
```

**Key insight:** The warm layer's *trigger metadata* lives in hot memory (skill names, index entries, applyTo globs). Only the *body* loads on demand. Pattern: **put the table of contents in hot, put the chapters in warm.**

> **Trigger mechanisms vary by agent:**
> - **Rule-based:** Copilot `applyTo` glob match, Codex skill metadata match
> - **Semantic:** CC/Gemini memory index — agent reads description and decides to load
> - **Explicit:** Agent calls `activate_skill` or `read_file` when task matches

> **Real-world example:** Claude Code's auto-memory system is a natural implementation of hot/warm separation — `MEMORY.md` index (hot, 200-line cap) points to individual `.md` memory files (warm, loaded when agent determines relevance from index description).

> **Common pattern:** CC, Codex, and Gemini all use hierarchical loading (global → project → subdir). This naturally supports "one file per responsibility" by placing topic-specific rules in the relevant subdirectory's instruction file.

---

## Agent-Specific File Mapping

> **Note:** Most agents are hybrid — they combine multiple loading models. The table below shows the primary mechanisms.

### Loading Models

| Model | Trigger | Examples |
|-------|---------|---------|
| **Always loaded** | Every session/interaction in repo context | Kiro `.kiro/steering/*`, CC/Codex/Gemini root instruction file, Copilot `.github/copilot-instructions.md` |
| **Directory-scoped** | Processing files within that directory tree | CC/Codex/Gemini subdir instruction files, Copilot `AGENTS.md` (nearest-in-tree) |
| **File-scoped** | Matching an `applyTo` glob pattern | Copilot `.github/instructions/**/*.instructions.md` |

**Implication for hot memory design:**
- "Always loaded" = put task-agnostic rules here (identity, verdict logic, workflow triggers)
- "Directory-scoped" = put domain-specific rules here (gateway checklist, docs standards)
- "File-scoped" = put file-type-specific review expectations here (only Copilot supports this natively)

| Agent | Hot Memory Location | Notes |
|-------|-------------------|-------|
| Kiro | `AGENTS.md` + `.kiro/steering/*.md` | Multiple files, one per topic |
| Claude Code | `CLAUDE.md` (project) + `~/.claude/CLAUDE.md` (global) + `MEMORY.md` index | Hierarchical loading (global → project → subdir). Auto-memory index is hot (200-line cap); individual memory files are cold. `settings.json` is config, not instructions |
| Codex | `AGENTS.md` hierarchical (global → project root → subdir) | Each directory loads at most one file. 32KB cap (`project_doc_max_bytes`). Use nested `AGENTS.md` for per-directory responsibility split. No multi-file topic split within same dir |
| Gemini | `GEMINI.md` hierarchical (`~/.gemini/GEMINI.md` global → `./GEMINI.md` project → subdir) + `MEMORY.md` index | Same hierarchical pattern as CC/Codex. Private project memory index is hot; individual memory files are cold |
| Copilot | `.github/copilot-instructions.md` (repo-wide) + `.github/instructions/**/*.instructions.md` (path-specific) + `AGENTS.md` (nearest-in-tree, where supported: cloud agent / CLI) | Layered: Personal > Path-specific > Repo-wide > Agent > Organization. No documented hard size cap for Chat/Agent (code review reads first 4K chars only). Keep short (~2 pages recommended) |
| OpenCode | `AGENTS.md` or equivalent | Follows repo convention |
| Pi | `AGENTS.md` hierarchical (project root → global) + `SYSTEM.md` or `APPEND_SYSTEM.md` in `.pi/` | Project or global `SYSTEM.md` replaces the default system prompt, while `APPEND_SYSTEM.md` appends to it. `AGENTS.md` is loaded hierarchically for context injection |

---

## Validation Checklist

After adding or changing hot memory:

1. **Start a fresh session** (no prior context)
2. **Ask a question that triggers the rule** — e.g., "what format should a review comment use?"
3. **Verify the response follows the rule exactly**
4. **Test edge cases** — e.g., "what if there's only one 🟡 finding?"
5. **Check for contradictions** — does the new rule conflict with anything else in hot memory?

If the agent doesn't follow the rule → it's either not loaded, too buried in other content, or ambiguously worded.

---

## Anti-Patterns

| Anti-Pattern | Why It's Bad | Fix |
|--------------|-------------|-----|
| Dumping everything into one file | Critical rules get lost in noise | Split by responsibility |
| Duplicating rules across files | Inevitable contradictions when one is updated | Single source + pointer |
| Putting case studies in hot memory | Wastes context budget on history | Move to docs, reference by lesson only |
| Vague rules ("be helpful") | Untestable, no behavioral change | Make specific and testable |
| Hot memory > 20KB | Diminishing returns, attention dilution | Audit and move cold items out |
| Task-scoped rules in file/directory-scoped locations | Review SOP, response format, collaboration protocol only load when certain files are touched — missing when needed most | Put task-agnostic workflow rules in always-loaded layer, not path-specific |
| Stale links in hot memory | Index points to missing files; fresh session gets dead references | Audit links quarterly; remove or create the target |
| Mandatory behavior hidden in cold without trigger | Agent must follow it but has no path to discover it | Add trigger metadata to hot, or promote to warm with clear activation condition |

---

## Maintenance

- **Quarterly audit**: Review hot memory files. Remove rules that are no longer relevant or have become default behavior.
- **After contradictions**: When agent behavior contradicts a rule, check if it's a loading issue or a conflict with another rule.
- **After new capabilities**: When adding new workflows, decide hot vs cold before writing the doc.
- **Adding a new agent**: Document its loading model and precedence before adding file mappings. Don't assume it works like existing agents.

---

## Self-Reflection Prompt

Use this prompt from a fresh session to audit memory allocation against this guide:

```
Per the steering design guide in docs/steering-design-guide.md from OpenAB GitHub repo, review your current memory allocation:

1. INVENTORY — List all loaded/discoverable instruction sources:
   - File path
   - Layer (Hot / Warm / Cold)
   - Trigger model (always / directory / file-glob / semantic / explicit)
   - Approximate size (lines or KB)

2. CLASSIFY — For each item, what type of content is it?
   - WHAT/HOW (behavior rule) vs WHY (history/rationale)
   - Identity / hard rule / workflow / reference / trivia

3. VIOLATIONS — Identify items that break the guide's principles:
   - Not behavior-oriented (nice-to-know in hot)
   - Duplicated or conflicting across files
   - Stale links pointing to missing files
   - Too large for its layer
   - WHY/history in hot instead of cold
   - Mandatory behavior in cold with no trigger path

4. TRIGGER QUALITY — Review warm layer triggers:
   - Are index descriptions precise enough to fire correctly?
   - Where is the canonical source for each rule?
   - Will the agent reliably see it when needed?

5. OPTIMIZATION PLAN — Propose concrete moves:
   - Remove (stale, duplicate, irrelevant)
   - Keep in hot (behavioral, high-frequency)
   - Promote cold → warm (add trigger)
   - Demote hot → warm or cold (too large, low-frequency)
   - Split (one file doing too many jobs)
   - Add missing trigger/index entry

6. VERIFY — Name one fresh-session test prompt that would confirm
   the highest-risk rule still loads correctly.
```

Expected output: a before/after table with file paths, layer assignments, sizes, and rationale for each move.

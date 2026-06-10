# ADR: Control Directives

- **Status:** Proposed
- **Date:** 2026-06-02
- **Author:** chaodu-agent
- **Related:** [Output Directives](../output-directives.md)

---

## 1. Context

### 1.1 Problem

A single OAB bot instance may serve multiple projects, each with its own steering files, skills, and workspace context. Today, there is no mechanism for a user to specify session-level parameters (working directory, model) when initiating a conversation. The bot always starts with its default configuration, requiring manual reconfiguration or separate bot instances per project.

### 1.2 Why Workspace Selection Matters

Specifying a workspace at session start eliminates the manual warm-up cost. Without it, users spend the first several messages orienting the agent. With `[[ws:...]]`, the agent gains full project context immediately:

- **Steering rules** — `AGENTS.md` and `.kiro/steering/` load automatically (coding style, architecture decisions, naming conventions). The agent knows how this project does things.
- **Skills** — `.kiro/skills/` activate project-specific capabilities (e.g., review workflows, deployment helpers).
- **File scope** — the agent's working directory is the project root; reads, searches, and writes target the correct codebase.
- **Git context** — branch, remote, and commit history are correct for PR review, diff, and blame operations.
- **Session isolation** — multiple projects served by one bot instance never leak context between sessions.

In short: one directive replaces 3–5 round trips of "here's the repo, here's how we work, here's what I need."

### 1.3 Existing Pattern

OAB already has **output directives** — `[[key:value]]` syntax that agents prepend to their responses to control delivery behavior (e.g., `[[reply_to:...]]`). This pattern is well-understood, parsed reliably, and invisible to end users after processing.

### 1.4 Opportunity

Extend the `[[key:value]]` convention to **input** (user → bot) messages, creating **control directives** that configure the session at creation time. This unifies the directive syntax across both directions and gives users declarative control over session initialization without requiring new slash commands or config files.

---

## 2. Decision

Introduce **Control Directives** — `[[key:value]]` patterns in user messages that configure session parameters. They share the double-bracket syntax with output directives but flow in the opposite direction (user → broker → agent runtime).

### 2.1 Syntax

```
@Bot [[ws:~/workdir/foo]] [[title:PR review]]
investigate this build failure
```

### 2.2 Core Rules

| Rule | Behavior |
|------|----------|
| **Scope** | Processed only on the session's first message (the one that mentions/triggers the bot) |
| **Parsing** | Extract a leading control header only: directives immediately after the trigger/mention and before normal prompt text |
| **Stop condition** | The first non-directive token or line starts prompt content; later `[[key:value]]` text is preserved verbatim |
| **Stripping** | Header directives are removed from the message; remaining text becomes the prompt |
| **Duplicate keys** | Last value wins |
| **Unknown keys** | Silently ignored (forward compatible) |
| **Placement** | Directives may be adjacent on the trigger line or on following header lines before prompt content begins |
| **Empty value** | Behavior is directive-specific: empty `ws` and `model` are invalid; empty `title` means "use generated title" |

### 2.3 Architecture Position

```
User message
     │
     ▼
┌─────────────────────┐
│  Directive Parser    │  ← extracts leading [[key:value]] header, strips it
│  (middleware)        │
└─────────────────────┘
     │
     ├── structured SessionMetadata
     │
     ▼
┌─────────────────────┐
│  Agent Runtime       │  ← receives clean prompt + metadata
└─────────────────────┘
```

The directive parser runs **before** the message enters the agent pipeline. It outputs:
- `prompt: String` — the message with directives stripped
- `metadata: SessionMetadata` — parsed key-value pairs for runtime configuration

---

## 3. Supported Directives (Phase 1)

| Directive | Purpose | Example |
|-----------|---------|---------|
| `[[ws:/path]]` | Set session working directory; loads steering/skills from that path | `[[ws:~/projects/myapp]]` |
| `[[title:...]]` | Set initial thread title | `[[title:Bug triage #42]]` |

### 3.1 `[[ws:/path]]` — Workspace

- Loads `AGENTS.md`, `.kiro/steering/`, and skill definitions from the target path
- If the path does not exist, session fails with user-visible error
- **Security boundary — bot home subtree only.** Enforcement order:
  1. Reject if path is relative (does not start with `~` or `/`)
  2. Expand `~` → bot home directory
  3. Canonicalize both the bot home root and target path (resolve symlinks, `..`, `.`) via `std::fs::canonicalize()` or equivalent
  4. Verify canonical path starts with bot home root (`canonical.starts_with(bot_home)`)
  5. **Reject** if outside bot home — session does not start, user-visible error returned
- The canonical workspace is stored in `SessionMetadata` and reused for session creation, session load/resume, eviction rebuilds, and any persisted session mapping. A `[[ws:...]]` session must never resume in the configured default working directory unless that was the resolved workspace.
- Workspace steering defines repo context (remote URL, branch, etc.) — no separate repo binding needed in Phase 1

#### Workspace Aliases

Full paths are verbose for frequent use. The operator may define workspace aliases in `config.toml`:

```toml
[workspace.aliases]
openab = "~/projects/openab"
infra  = "~/projects/infra-cdk"
web    = "~/projects/frontend"
```

Users reference aliases with an `@` prefix:

```
@Bot [[ws:@openab]] [[title:Fix CI]]
help me debug the smoke test
```

Resolution order:
1. If value starts with `@`, look up alias in `workspace.aliases`
2. If alias not found → reject with user-visible error listing available aliases
3. If found → substitute the full path, then apply the same canonicalize + security check as raw paths

Aliases are syntactic sugar — they resolve before any security validation and produce identical `SessionMetadata` to raw paths.

### 3.2 `[[title:...]]` — Thread Title

- Sets the initial thread/channel title
- Agent may override this later per its own SOP (e.g., status-based title updates)
- Max length: 100 characters (truncated silently)

### 3.3 `[[model:...]]` — Model Selection

Phase 2 adds `[[model:...]]` after the parser, workspace, and title path are implemented.

- Value must match a configured model identifier
- If the model is unavailable or unknown, the session is rejected after runtime config discovery but before the user's first prompt is sent. The user receives an error and no user task is executed in the wrong model.
- Does not persist beyond the session

---

## 4. Design Decisions

### 4.1 Why Session-First Only

Processing directives only on the first message keeps the mental model simple:
- No mid-conversation state mutations
- No need for "directive acknowledged" confirmation messages
- Session parameters are immutable once established
- Easier to reason about for both users and agents

### 4.2 Why Not Slash Commands

| Aspect | Slash Commands | Control Directives |
|--------|---------------|-------------------|
| Discovery | Platform UI autocomplete | Docs / muscle memory |
| Composability | One command at a time | Multiple directives in one message |
| Platform dependency | Requires registration per platform | Platform-agnostic (just text) |
| Works with mention | Awkward (`/command @bot`) | Natural (`@bot [[...]] prompt`) |

Control directives are platform-agnostic text — they work on Discord, Slack, Telegram, or any adapter without platform-specific command registration.

### 4.3 Relationship to Output Directives

| Aspect | Output Directives | Control Directives |
|--------|-------------------|-------------------|
| Direction | Agent → Platform | User → Broker |
| Processing layer | Response post-processor | Message pre-processor |
| Timing | Every response | Session first message only |
| Syntax | `[[key:value]]` | `[[key:value]]` |
| Unknown keys | Ignored | Ignored |
| Duplicate keys | Last wins | Last wins |

Shared syntax reduces cognitive load. The direction is unambiguous from context (who authored the message).

Control directives intentionally do **not** scan the entire message body. This mirrors the output directive header concept and avoids corrupting normal prompts that quote code, issue templates, or examples containing `[[key:value]]`.

### 4.4 Security Considerations

- `[[ws:...]]` enforces bot home subtree only — canonicalize, reject symlink escapes (see §3.1)
- `[[model:...]]` only selects from pre-configured models; cannot inject arbitrary API endpoints; unknown model = hard fail
- Directive values are sanitized (no newlines, no control characters beyond the value delimiter)

### 4.5 No Mid-Session Reset

Control directives are immutable once the session starts. There is no mechanism to change `ws`, `title`, or `model` mid-conversation. To change parameters, start a new session. This eliminates state mutation complexity and keeps the session contract predictable.

---

## 5. Future Extensions

These are **not** in scope for Phase 1 but the design accommodates them:

| Directive | Purpose |
|-----------|---------|
| `[[repo:owner/repo]]` | Bind GitHub repository context (Phase 1 relies on `[[ws:...]]` steering to define repo context) |
| `[[timeout:300]]` | Session timeout in seconds |
| `[[skill:review]]` | Activate a specific skill set |
| `[[label:bug]]` | Tag the session/thread with labels (multi-value: array semantics) |

**Why `[[repo:...]]` is not in Phase 1:** Workspace steering files already define repository context (remote URL, branch conventions, etc.). A standalone `[[repo:...]]` directive would need to specify what "binding" means (clone? set remote? just metadata?) — that design is deferred until usage patterns emerge from `[[ws:...]]` adoption.

For multi-value keys (e.g., `[[label:a]] [[label:b]]`), a future revision may introduce array semantics where repeated keys accumulate rather than overwrite. Phase 1 uses last-wins for all keys.

---

## 6. Implementation Plan

### Phase 1: Parser + `ws` (with aliases) + `title`

1. Implement directive parser as a middleware in the message ingestion pipeline
2. Define `SessionMetadata` struct
3. Persist `SessionMetadata` per session key so workspace survives reconnect, resume, and eviction rebuilds
4. Add `[workspace.aliases]` table to `config.toml` schema
5. Wire `[[ws:...]]` to workspace/context loading (raw paths and `@alias` resolution)
6. Wire `[[title:...]]` to thread title initialization
7. Unit tests for parser edge cases (nested brackets, escaped content, empty values, body text that contains directive-like literals, unknown alias error)

### Phase 2: `model`

1. Wire `[[model:...]]` to model selection in agent runtime
2. Validate against runtime config options before the first user prompt is sent; reject unknown values instead of falling back silently

### Phase 3: `/new` Slash Command

Platform-specific UX sugar that translates to control directives internally.

```
/new ws:~/projects/myapp model:claude-sonnet-4-20250514
investigate the build failure
```

1. Register `/new` slash command on supported platforms (Discord, Slack)
2. Command handler parses arguments into `[[key:value]]` directives
3. Feeds through the same directive parser pipeline as inline directives
4. Creates a new thread with the parsed session metadata

**Why `/new`:**
- Short, intuitive — matches "new session/thread" mental model
- **Typed arguments with platform UI** — autocomplete for workspaces, dropdown for models, validation before submit. Users don't need to memorize exact model names or path syntax
- Does not conflict with other bots' commands
- Naturally implies "session start" — aligns with first-message-only rule

**Relationship to inline directives:**
- `/new` is **transport sugar only** — it MUST NOT introduce semantics beyond what `[[key:value]]` provides
- Users who prefer text-only (or are on platforms without slash commands) use `@Bot [[...]]` directly
- Both paths produce identical `SessionMetadata`
- `/new` and inline `[[...]]` cannot co-exist in the same message (a `/new` invocation IS the session's first message; there is no separate text body to embed inline directives)

---

## 7. Alternatives Considered

| Alternative | Rejected Because |
|-------------|-----------------|
| YAML front-matter in messages | Visually heavy; unfamiliar to chat users |
| Separate `/config` command before conversation | Extra round-trip; breaks single-message session start |
| Per-channel bot configuration | Doesn't scale to ad-hoc project switching |
| Environment variables per bot instance | Requires multiple bot deployments |

---

## 8. References

- [Output Directives](../output-directives.md) — existing `[[key:value]]` pattern for agent → platform
- [Steering Design Guide](../steering-design-guide.md) — how workspace steering files are structured

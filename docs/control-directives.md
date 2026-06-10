# Control Directives

## Overview

Users can configure session-level parameters by adding `[[key:value]]` directives to their first message. These are parsed and stripped before the prompt reaches the agent.

## Format

```
@Bot [[ws:@openab]] [[title:Fix CI workflow]]
help me debug the smoke test failures
```

Rules:
- Directives are only processed on the **first message** of a session
- Consecutive `[[key:value]]` tokens at the start = directive header
- First non-directive token/line = prompt content begins
- Later `[[key:value]]` text in the message body is preserved verbatim
- Duplicate keys: last value wins
- Unknown keys: silently ignored (forward compatible)
- Directives may span multiple lines:
  ```
  [[ws:@openab]]
  [[title:Review PR]]
  please review this change
  ```

## Available Directives

### `ws` — Workspace

Set the session's working directory. The agent starts with this path as cwd, loading steering files and skills from it.

```
[[ws:~/projects/myapp]]
[[ws:@openab]]
```

**Alias syntax:** Use `@name` to reference a configured alias (see [Workspaces](workspaces.md)).

**Security:**
- Path must be absolute (`~` or `/` prefix)
- `~` expands to bot home directory (`$HOME`)
- Path must exist and be a directory
- Path must be within bot home subtree (symlink escapes caught by canonicalization)
- Invalid paths abort the session with a user-visible error

### `title` — Thread Title

Set the initial thread/channel title (max 100 characters, truncated silently).

```
[[title:Bug triage #42]]
```

- Empty value (`[[title:]]`) = use auto-generated title
- The agent may override this later per its own workflow

## Behavior

- Directives are **immutable** — once a session starts, parameters cannot be changed mid-conversation
- To change workspace or title, start a new session
- If no `[[ws:...]]` is specified, the session uses the bot's default working directory
- If workspace resolution fails on a new session, the session is not created. However, `[[title:...]]` is applied independently before workspace validation — the thread title may already be set even if the session aborts.

## Relationship to Output Directives

| Aspect | Output Directives | Control Directives |
|--------|-------------------|-------------------|
| Direction | Agent → Platform | User → Bot |
| Timing | Every response | First message only |
| Syntax | `[[key:value]]` | `[[key:value]]` |

## Future Directives (Phase 2+)

| Directive | Purpose |
|-----------|---------|
| `[[model:...]]` | Select model for the session |
| `[[repo:owner/repo]]` | Bind GitHub repository context |
| `[[timeout:300]]` | Session timeout in seconds |

## See Also

- [Workspaces](workspaces.md) — configuring workspace aliases
- [Output Directives](output-directives.md) — agent → platform directives
- [ADR: Control Directives](adr/control-directives.md) — design rationale

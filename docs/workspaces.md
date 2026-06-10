# Workspaces

## Overview

A single OAB bot instance can serve multiple projects. Workspaces let users switch project context at session start using the `[[ws:...]]` [control directive](control-directives.md).

When a workspace is set, the agent:
- Uses the workspace path as its working directory
- Loads steering rules from `AGENTS.md` and `.kiro/steering/`
- Activates skills from `.kiro/skills/`
- Has correct git context (branch, remote, history)

## Configuration

Define workspace aliases in `config.toml`:

```toml
[workspace.aliases]
openab = "~/projects/openab"
infra  = "~/projects/infra-cdk"
web    = "~/projects/frontend"
```

Paths starting with `~` expand to the bot's home directory (`$HOME`).

## Usage

Reference aliases with `@` prefix in the first message:

```
@Bot [[ws:@openab]] help me debug the smoke tests
```

Or use raw paths:

```
@Bot [[ws:~/projects/myapp]] investigate the build failure
```

## Security Boundary

All workspace paths are validated before use:

1. **Must be absolute** — relative paths (e.g., `../secrets`) are rejected
2. **`~` expands to bot home** — not the requesting user's home
3. **Canonicalized** — symlinks, `..`, `.` are resolved
4. **Must be within bot home subtree** — paths outside are rejected
5. **Must be a directory** — file paths are rejected
6. **Must exist** — non-existent paths are rejected with a clear error showing the expanded path

## Session Behavior

- Workspace is set **once** at session creation and is immutable
- The workspace persists across session suspend/resume and eviction rebuilds
- To change workspace, start a new session
- If workspace resolution fails, no session is created (clean failure)

## Error Messages

| Scenario | Error |
|----------|-------|
| Unknown alias | `Unknown workspace alias @foo. Available: openab, infra, web` |
| Relative path | `Workspace path must be absolute (start with ~ or /): relative/path` |
| Outside home | `Workspace path is outside allowed directory: /etc/passwd` |
| Not a directory | `Workspace path is not a directory: /home/bot/Cargo.toml` |
| Does not exist | `Workspace path does not exist: ~/nope (expanded to /home/bot/nope)` |

## See Also

- [Control Directives](control-directives.md) — full directive syntax and rules
- [Config Reference](config-reference.md#workspace) — `[workspace.aliases]` configuration

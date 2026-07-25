# CLI Conventions (`openab`)

One rule governs the `openab` command surface:

> **Top-level verbs act on the bot itself; noun namespaces act on
> subsystems.**

## Top-level verbs — the bot's own lifecycle and control

```
openab run [-c config]    # run the bot (default command)
openab setup              # interactive config wizard
openab set <key> <value>  # ctl-socket write to the running bot (e.g. thread.name)
openab get <key>          # ctl-socket read from the running bot
```

These are frozen. New top-level verbs require the same justification: the
action must target the bot process itself, not a component of it.

## Noun namespaces — subsystems and add-ons

```
openab mcp <addon> <action> [flags]
# e.g.
openab mcp gmail-native login
openab mcp gmail-native serve --listen 127.0.0.1:8850
```

Everything that operates on a subsystem (MCP add-ons today; future
subsystems follow the same shape) lives under a noun namespace. Namespace
subcommands are for **interactive and operational actions** (logins,
standalone/dev serving, diagnostics).

## Long-running serving is config-driven, not CLI-driven

Production serving belongs to `openab run` + `config.toml` — presence of a
section enables the component (e.g. `[mcp]` starts the OAB MCP Facade
listener). CLI `serve` actions under a namespace exist for development and
standalone hosts, never as the production path (see #1451 for the
facade-only run mode).

## Precedent

This mirrors how mature CLIs converged: `docker` keeps top-level lifecycle
shortcuts (`docker run`) alongside management namespaces
(`docker image ls`); `gh` scopes everything by noun (`gh pr create`,
`gh auth login`). Mixed is fine — undocumented mixed is not.

## Notes

- `set`/`get` are semantically ctl-client operations; a purist rename
  (`openab ctl set`) is deliberately not pursued — they are shipped and
  scripted against. If purity is ever wanted, add aliases, never rename.
- `openab-agent` is the coding agent + MCP *client* toolchain
  (`openab-agent mcp list|status|doctor|connect|login`); serving surfaces
  do not belong on it (#1451).

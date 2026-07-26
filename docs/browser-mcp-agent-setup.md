# Browser MCP — how the agent gets the browser tools

The browser MCP server exposes five DOM-semantic tools —
`katashiro.read_dom`, `katashiro.screenshot`, `katashiro.navigate`, `katashiro.click`,
`katashiro.type` — served by the **browser extension** over the MCP-over-ACP tunnel (see
[tunnel contract](./mcp-over-acp-tunnel-contract.md)). This doc covers the *other* hop: how the
colocated agent CLI actually **sees** those tools.

There are three transports, selected by `OPENAB_BROWSER_MODE`. **Facade is the default and the one
to use**; `proxy` and `bridge` predate it and are kept as explicit opt-outs.

| mode | how the agent reaches the tools | status |
|---|---|---|
| unset / `facade` | through the **OAB MCP Facade**, one listener, static config entry | **default** |
| `proxy` | per-session loopback MCP server + per-session config rewrite | legacy opt-out |
| `bridge` | per-pod unix socket + `openab browser-bridge` stdio relay (Option C) | legacy opt-out |

With no `[mcp]` section in `config.toml` the facade is not serving, and facade mode falls back to
`proxy` automatically.

---

## Facade mode (default)

Browser tools are a **session-aware in-process capability source** of the
[OAB MCP Facade](./oab-mcp-facade.md) — the same aggregation point that serves every other
provider in `mcp.json`. Enable the facade and it works:

```toml
# config.toml
[mcp]
listen = "127.0.0.1:8848"

# Operator gate for client-declared type:acp servers (reverse-MCP ADR §6.4).
# Omit entirely to keep the built-in default: `katashiro` only, pinned to its five tools.
[[mcp.acp_servers]]
name  = "katashiro"
tools = ["katashiro.read_dom","katashiro.screenshot","katashiro.navigate","katashiro.click","katashiro.type"]
```

- **One listener** — the facade's. No per-session ports and no per-session config rewrites.
- **Identity** — the pool mints one token per chat session and injects it into the agent process as
  `OPENAB_SESSION_TOKEN`. The MCP config entry openab writes is **static and write-once**, and
  references the variable rather than embedding a secret:

  ```json
  {
    "mcpServers": {
      "openab": {
        "url": "http://127.0.0.1:8848/mcp",
        "headers": { "Authorization": "Bearer ${OPENAB_SESSION_TOKEN}" }
      }
    }
  }
  ```

  Tokens are revoked on session evict; calls route to that session's browser over the same
  `channel_id` tunnel. Because the secret rides the process environment, it never lands in a file
  a shared workdir could expose.
- **Discovery** — the agent does **not** see `katashiro.*` in its own `tools/list`. It sees the
  facade's two meta-tools and finds browser tools through `search_capabilities`, then runs them via
  `execute_capability`, alongside every other facade capability. A session-bound source is invisible
  to anonymous facade clients — no token, no discovery, no execution.

### Any MCP-capable CLI works

Because the entry is static, **hand-configuring a variant openab does not auto-write is viable**:
point the CLI at `http://127.0.0.1:8848/mcp` with the bearer header above. This is the practical
difference from proxy mode, where the endpoint was per-session ephemeral and a hand-written entry
went stale on the next session.

### Verify

```sh
# facade listening?
grep "OAB MCP facade listening" <agent logs>

# the static entry openab wrote
cat "$HOME/.cursor/mcp.json"            # Cursor
cat "$HOME/.kiro/settings/mcp.json"     # Kiro

# does the catalog contain the browser capabilities for a session-bound client?
#   -> call search_capabilities from the agent; expect provider "openab-browser"
#      with exactly the pinned katashiro.* tools
```

Gateway log confirms the extension side: `ACP: browser tunnel registered — extension attached`.

---

## Legacy: `proxy` mode

`OPENAB_BROWSER_MODE=proxy` forces the original design: the gateway↔extension tunnel terminates at a
**per-session loopback MCP proxy** (`openab-core` `mcp_proxy::start_session_server`), and openab
writes a per-session entry into the agent CLI's native MCP config:

```json
{
  "mcpServers": {
    "openab-browser": {
      "url": "http://127.0.0.1:<port>/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

`<port>` and `<token>` are **minted fresh per session** and the entry is stripped on evict, so it
cannot be hand-written to a fixed value. In this mode the agent sees `katashiro.*` directly in its
`tools/list` rather than behind the facade's meta-tools.

Config file per variant (what `start_session_server` writes):

| Variant | MCP config file (under `$workdir`, = `$HOME`) | Auto-written in proxy mode |
|---|---|---|
| **Cursor** (`cursor-agent`) | `.cursor/mcp.json` | ✅ |
| **Kiro** (`kiro-cli`) | `.kiro/settings/mcp.json` | ✅ |
| **Claude Code** | `.mcp.json` / `~/.claude.json` `mcpServers` | ⛔ |
| **Codex** | `~/.codex/config.toml` `[mcp_servers.*]` (TOML) | ⛔ |
| **Gemini CLI** | `~/.gemini/settings.json` `mcpServers` | ⛔ |

Variants marked ⛔ are unreachable in proxy mode without teaching `start_session_server` their
config path and format — **or simply using facade mode**, where the static entry removes the problem.

## Legacy: `bridge` mode (Option C)

`OPENAB_BROWSER_MODE=bridge` runs a per-pod unix-socket server plus an `openab browser-bridge`
stdio-MCP relay; the CLI entry is the static `{"command":"openab","args":["browser-bridge"]}` and the
relay resolves its session channel by process ancestry. Intended for stdio-only MCP clients.

> Retiring proxy and bridge once facade mode has soaked is tracked as a follow-up — the OAB MCP
> Adapter ADR's Alternative C calls for a single agent-facing aggregation point.

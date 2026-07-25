# Browser MCP — how the agent gets the `openab-browser` tools

The browser MCP server exposes five DOM-semantic tools —
`browser.read_dom`, `browser.screenshot`, `browser.navigate`, `browser.click`, `browser.type` —
served by the **browser extension** over the MCP-over-ACP tunnel (see
[tunnel contract](./mcp-over-acp-tunnel-contract.md)). This doc covers the *other* hop: how the
colocated agent CLI actually **sees** those tools.

## How it reaches the agent

The gateway↔extension tunnel terminates in the pod at a **per-session loopback MCP proxy**
(`openab-core` `mcp_proxy::start_session_server`). To expose it to the agent, openab writes an
`openab-browser` entry into the agent CLI's **native MCP config file** — the agent connects to
`http://127.0.0.1:<port>/mcp` and re-lists the tools. The entry looks like:

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

Both `<port>` and `<token>` are **minted fresh per session** and the entry is stripped on session
evict — so this is written by openab, not hand-editable to a fixed value (see *Caveat* below).

## Per-variant MCP config location

openab writes the same entry into whichever file the colocated CLI reads. Current state:

| Variant | MCP config file (under `$workdir`, = `$HOME`) | HTTP MCP + `headers` | Auto-written by openab today |
|---|---|---|---|
| **Cursor** (`cursor-agent`) | `.cursor/mcp.json` | yes | ✅ yes |
| **Kiro** (`kiro-cli`) | `.kiro/settings/mcp.json` | yes | ✅ yes |
| **Claude Code** | `.mcp.json` / `~/.claude.json` `mcpServers` | yes | ⛔ not yet |
| **Codex** | `~/.codex/config.toml` `[mcp_servers.*]` (TOML) | check version | ⛔ not yet |
| **Gemini CLI** | `~/.gemini/settings.json` `mcpServers` | yes | ⛔ not yet |

`start_session_server` currently writes **`.cursor/mcp.json` + `.kiro/settings/mcp.json`**. Adding
a variant = teach that function the CLI's config path + format (same `{url, headers}` shape for
JSON configs; Codex uses TOML and needs a small serializer).

## Manual / unsupported variants

If a variant isn't auto-written, a user *could* add the `openab-browser` entry to that CLI's
mcp.json by hand — **but** the current proxy endpoint is per-session ephemeral (fresh port +
bearer each session), so a static hand-written entry goes stale on the next session and cannot
be used as-is. Manual configuration for arbitrary variants is therefore gated on a **stable
browser-MCP endpoint** (a fixed URL + stable auth the user configures once). That redesign is
tracked separately (see `drafts/` browser-MCP stable-endpoint design). Until then:

- **Cursor / Kiro:** work out of the box (auto-injected).
- **Other variants:** either add the variant's writer to `start_session_server`, or wait for the
  stable endpoint.

## Verify

Inside the agent pod, after the extension attaches a browser session:

```sh
# the entry openab wrote for this session
cat "$HOME/.cursor/mcp.json"                 # Cursor
cat "$HOME/.kiro/settings/mcp.json"          # Kiro

# does the CLI see the server / tools?  (CLI-specific; e.g. Kiro:)
kiro-cli mcp list
```

Gateway log confirms the extension side:
`ACP: browser tunnel registered — extension attached`.

## Facade mode (default when `[mcp]` is enabled)

With the OAB MCP Facade running (`[mcp]` in `config.toml`), browser tools are
served as a **session-aware in-process capability source** of the facade
instead of per-session proxy servers:

- **One listener** (the facade's, e.g. `127.0.0.1:8848/mcp`) — no per-session
  ports, no per-session config rewrites.
- **Identity**: the pool mints one token per chat session and injects it as
  `OPENAB_SESSION_TOKEN` into the agent process environment; the (static,
  write-once) MCP config entry references it as
  `"Authorization": "Bearer ${OPENAB_SESSION_TOKEN}"`. Tokens are revoked on
  session evict; calls route to that session's browser via the same
  `channel_id` tunnel contract as proxy mode.
- **Discovery**: agents find browser tools through `search_capabilities`
  alongside every other facade capability, and execute them via
  `execute_capability`.

Mode selection (`OPENAB_BROWSER_MODE`): unset/`facade` → facade routing when
the facade is serving, with automatic fallback to `proxy` when it is not
(no `[mcp]` section); `proxy` → force the original per-session loopback
servers; `bridge` → Option C stdio bridge. Proxy and bridge behavior is
unchanged.

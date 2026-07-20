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

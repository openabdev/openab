# Browser MCP — how the agent gets the browser tools

The browser MCP server exposes five DOM-semantic tools —
`katashiro.read_dom`, `katashiro.screenshot`, `katashiro.navigate`, `katashiro.click`,
`katashiro.type` — served by the **browser extension** over the MCP-over-ACP tunnel (see
[tunnel contract](./mcp-over-acp-tunnel-contract.md)). This doc covers the *other* hop: how the
colocated agent CLI actually **sees** those tools.

There is **one** transport: the OAB MCP Facade. The per-session `proxy` and the `openab
browser-bridge` stdio relay both existed before it and have been removed, along with the
`OPENAB_BROWSER_MODE` variable that selected between them. See [Removed
transports](#removed-transports) if you are upgrading from either.

**Browser control now requires `[mcp]` in `config.toml`.** This is a breaking change. Without that
section there is no browser control — openab does **not** start a listener you did not configure,
and says so once at startup rather than leaving you to infer it from missing tools.

> ⚠️ **A leftover `openab-browser` entry from either old transport can still sit in your agent's
> `mcp.json`,** and the two are handled differently.
>
> The **bridge** entry is deleted for you on the next session — in `.cursor/mcp.json`,
> `.kiro/settings/mcp.json` and the kiro agent files, including its `@openab-browser` grant. Left
> in place it names a subcommand that no longer exists, so the agent's MCP client would try and
> fail to start it every session. It is removed only when byte-identical to the entry we wrote
> (`{"command":"openab","args":["browser-bridge"]}`), because that exact shape is the only proof we
> have that it is ours rather than a server you configured under the same key.
>
> The **proxy** entry is deliberately *not* removed: its url and bearer were minted per session and
> never recorded anywhere, so under that key we cannot tell your server from ours. It is dead
> configuration — the port died with its session — but if you want it gone, remove it yourself:
>
> ```sh
> # edits in place; check the diff before trusting it
> for f in "$HOME/.cursor/mcp.json" "$HOME/.kiro/settings/mcp.json"; do
>   [ -f "$f" ] && jq 'del(.mcpServers["openab-browser"])' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
> done
> ```

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

## Removed transports

Both legacy transports are gone. This section is kept as a migration note, not as documentation of
anything you can still turn on.

**`proxy` mode** terminated the gateway↔extension tunnel at a per-session loopback MCP server and
rewrote the agent CLI's MCP config with a freshly minted url and bearer on every session. It is
removed: the per-session server, its bearer, and the per-session config write and cleanup.

**`bridge` mode (Option C)** ran a per-pod unix-socket server plus an `openab browser-bridge`
stdio-MCP relay that resolved its session channel by walking `/proc`. It is removed: the
subcommand, the socket server, the ancestry resolver and the static config entry.

`OPENAB_BROWSER_MODE` no longer selects anything and can be unset. If it is still set to any value,
openab logs one warning at startup naming the value and reporting what is actually in force —
`facade` when `[mcp]` is configured, `disabled` when it is not.

**What to do when upgrading:** configure `[mcp]` in `config.toml`. Without it there is no browser
control at all — nothing is auto-started, because starting a listener you did not ask for is the
coupling this design deliberately avoids. A leftover `openab-browser` bridge entry is deleted for
you on the next session; a leftover proxy entry is not, for the ownership reason given above.

Proxy mode also only ever auto-wrote two of the five CLI variants (Cursor and Kiro); the other
three had to be configured by hand. The facade's static entry removes that gap rather than
extending it.

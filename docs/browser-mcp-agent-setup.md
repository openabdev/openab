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
section there is no browser control — openab does **not** start a listener you did not configure.

openab reports which of the two you are in once at startup, so it is never something you have to
infer from tools that do not appear:

```
INFO browser control: enabled via the OAB MCP Facade ([mcp] configured)
INFO browser control: unconfigured — no [mcp] section in config.toml, so browser tools are
     unavailable and nothing was started. Add [mcp] to enable them.
```

> ⚠️ **A leftover `openab-browser` entry from either old transport can still sit in your agent's
> `mcp.json`,** and the two are handled differently.
>
> **You have to remove it yourself. openab no longer edits your MCP config at all**, so neither
> entry is cleaned up for you.
>
> Earlier versions deleted the **bridge** entry on the next session. That cleanup worked by
> editing your file, and openab no longer does that — it authors `.openab/mcp-facade.json` and
> nothing else. **This is worth acting on rather than ignoring:** a leftover `openab-browser`
> entry is a *working* path to the browser that does not pass through facade policy or the audit
> trail, so until you remove it the allowlist in `[[mcp.acp_servers]]` is not the only way in. The
> bridge entry also names a subcommand that no longer exists, so the agent's MCP client will fail
> to start it every session.
>
> The **proxy** entry was never removed automatically either: its url and bearer were minted per
> session and never recorded, so under that key openab cannot tell your server from its own.
>
> Remove both, and the kiro agent-file grant that made the bridge entry callable:
>
> These delete the entry only when it is **byte-identical to the bridge entry openab wrote**
> (`{"command":"openab","args":["browser-bridge"]}`). That exact shape is the only proof it is ours
> rather than a server you configured under the same key — the automation this replaces used the
> same test, and a manual step should not be more destructive than the automation it stands in for.
>
> ```sh
> # edits in place; check the diff before trusting it
> BRIDGE='{"command":"openab","args":["browser-bridge"]}'
> for f in "$HOME/.cursor/mcp.json" "$HOME/.kiro/settings/mcp.json"; do
>   [ -f "$f" ] && jq --argjson bridge "$BRIDGE" \
>     'if .mcpServers["openab-browser"] == $bridge then del(.mcpServers["openab-browser"]) else . end' \
>     "$f" > "$f.tmp" && mv "$f.tmp" "$f"
> done
> # kiro agent files carry a separate default-deny grant; the entry stays reachable while it is listed
> for f in "$HOME"/.kiro/agents/*.json; do
>   [ -f "$f" ] && jq --argjson bridge "$BRIDGE" \
>     'if .mcpServers["openab-browser"] == $bridge
>      then del(.mcpServers["openab-browser"])
>           | .allowedTools = ((.allowedTools // []) - ["@openab-browser"])
>      else . end' \
>     "$f" > "$f.tmp" && mv "$f.tmp" "$f"
> done
> ```
>
> If your entry under that key is a *different* shape, these leave it alone — openab cannot tell it
> from a server of yours, which is why the proxy entry was never removed automatically either.

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
- **openab writes ONE file, and it is not yours.** It authors `<workdir>/.openab/mcp-facade.json`
  and never reads, merges into, or writes `.cursor/mcp.json`, `.kiro/settings/mcp.json` or a kiro
  agent file. Putting that entry in front of your agent is your step — see
  [Wiring it up](#wiring-it-up) below.
- **Identity** — the pool mints one token per chat session and injects it into the agent process as
  `OPENAB_SESSION_TOKEN`. The entry is **static**, and references the variable rather than
  embedding a secret:

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

### Wiring it up

openab authors `<workdir>/.openab/mcp-facade.json` and stops there. It does not run your agent's
CLI for you either — configuring your own agent stays your decision.

| Agent | What you do |
|---|---|
| **kiro** | `kiro-cli mcp import --file <workdir>/.openab/mcp-facade.json workspace` — the vendor performs the merge with its own semantics. Do **not** pass `--force`: it would overwrite a same-named server of yours. |
| **cursor** | No import mechanism exists — there is no include/extends and no launch flag. Paste the `mcpServers` object below into `.cursor/mcp.json` yourself. |
| **any other MCP-capable CLI** | Point it at `http://127.0.0.1:8848/mcp` with the bearer header above. Because the entry is static, a hand-written one keeps working — the practical difference from proxy mode, where the endpoint was per-session ephemeral and a hand-written entry went stale on the next session. |

The startup log prints the resolved path and these commands, so the value is not guessed from this
page.

### Verify

```sh
# facade listening?
grep "OAB MCP facade listening" <agent logs>

# the file openab authored (the only one it writes)
cat "$HOME/.openab/mcp-facade.json"

# and whether YOU have put it in front of the agent yet
cat "$HOME/.cursor/mcp.json"            # Cursor — you paste it here
cat "$HOME/.kiro/settings/mcp.json"     # Kiro — written by `kiro-cli mcp import`

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

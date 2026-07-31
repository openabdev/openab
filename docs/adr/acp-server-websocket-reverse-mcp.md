# ADR: Reverse MCP-over-ACP over WebSocket

- **Status:** Accepted — the mechanism and the generic multi-server generalization (§6) are both
  **as-built in #1447** (F1′, F3′, F4 and F5 landed; F6 e2e coverage remains).
- **Date:** 2026-07-18 (updated 2026-07-24)
- **Author:** @brettchien
- **Related:** [ACP Server over WebSocket — Base (as-built)](./acp-server-websocket-base.md),
  [ACP Server with WebSocket Transport](./acp-server-websocket.md) (original proposal),
  [openab-agent MCP](./openab-agent-mcp.md).
  The browser extension's implementation contract:
  [MCP-over-ACP tunnel contract](../mcp-over-acp-tunnel-contract.md).

---

## 1. Context

This ADR records **reverse MCP-over-ACP**: a mechanism that lets an ACP **WebSocket client** —
one that cannot open a listening socket — nevertheless act as an **MCP server**, serving its
tools to a colocated agent over the outbound `/acp` WS it already holds. OpenAB core is the MCP
proxy/aggregator in the middle; the agent is a normal in-pod MCP client.

The first, driving consumer is **browser control**: a browser side-panel extension serves DOM
tools so the agent's LLM can autonomously operate the user's real, logged-in Chrome (see **§7** for that
concrete design and the extension contract). This ADR describes the general mechanism and its generalization to **multiple,
arbitrary** client-side MCP servers (§6), using browser control as the running example.

## 2. Decision

Expose a client-side capability as **MCP tools** and route them to the agent via **MCP-over-ACP**,
tunnelled over the **existing `/acp` WebSocket** the client already holds.

Why MCP (not a custom ACP `ExtRequest`): for the LLM to *autonomously* use a capability, its
actions must appear in the agent's tool list (`tools/list`) so the model discovers and calls them.
A custom `ExtRequest` is a transport-level ACP extension the LLM never sees as a tool — it only
fits OpenAB-driven (non-LLM) operations. MCP is the standard way agents receive tools.

### Roles
- **ACP WS client = MCP server (role/logic).** It handles `tools/list` / `tools/call` and executes
  the actions. A client that cannot open a *listening* socket (e.g. an MV3 browser extension) can
  still be an MCP server — MCP server/client is about *who provides tools*, not who opens the
  connection — so it serves MCP over the **outbound `/acp` WS it already opened**. This is the only
  way a can't-listen client can be a full MCP server.
- **OpenAB core = MCP proxy/aggregator.** A middlebox between two connections: it consumes the
  client's tools from the upstream tunnel and re-exposes them to the agent downstream. Note the LLM's
  own `tools/list` does **not** show them: it sees the facade's two meta-tools, and reaches the
  client's tools through `search_capabilities` / `execute_capability` (§6.3).
- **Agent = MCP client.** The agent (Claude / Codex / Cursor / Kiro …) is a subprocess colocated in
  the OpenAB pod; it calls the tools over its in-pod MCP link.

### One WebSocket, multiplexed
The single `/acp` WS carries BOTH the ACP chat session (initialize / session.prompt /
session.update) AND the tunnelled MCP traffic (tools/list / tools/call / results), distinguished by
ACP method namespace. No second connection. This multiplexing applies to the **upstream** hop
(client ↔ gateway), using the official MCP-over-ACP `mcp/message` framing. The **downstream** hop
(core ↔ agent) is *not* tunnelled over ACP — core hosts a normal in-process MCP server the agent
connects to; only the client, which cannot listen, needs MCP tunnelled over its `/acp` WS.

## 3. Protocol gap to close first

The base does only client→agent (prompt) and agent→client **notifications** (streaming text).
Reverse MCP needs the **agent→client REQUEST** direction (request/response: the agent asks the
client to do X and awaits a result). The WS is already bidirectional; `acp_server`'s dispatch loop
adds the agent-initiated-request path. This is also where the wire types move from hand-rolled to
**generated** (see §9).

## 4. Architecture (browser control as the example)

```mermaid
flowchart LR
  EXT["<b>Side-panel MV3 extension</b> = MCP SERVER<br/>(cannot open a listening socket → serves MCP<br/>over the outbound /acp WS it already holds)<br/>tools: read_dom · screenshot · navigate · click · type"]
  subgraph POD["OPENAB POD — 'openab run', one process tree"]
    direction LR
    GW["<b>openab-gateway</b><br/>/acp WS server<br/>AcpTunnelRegistry"]
    CORE["<b>openab-core</b><br/>OAB MCP Facade"]
    AGENT["<b>agent CLI</b><br/>Cursor · Kiro · Claude · Codex<br/>LLM = MCP CLIENT"]
    GW <--> CORE
    CORE ==>|"<b>OAB MCP Facade</b> (only path)<br/>one listener, requires [mcp]<br/>openab authors .openab/mcp-facade.json — operator wires it in<br/>static {url, Bearer ${OPENAB_SESSION_TOKEN}}<br/>token in agent env, revoked on evict"| AGENT
  end
  EXT <==>|"UPSTREAM — only remote hop<br/>MCP-over-ACP · mcp/message framing<br/>multiplexed with ACP chat on ONE /acp WSS<br/>8 MiB frame cap · JPEG screenshots"| GW
  classDef remote fill:#fde68a,stroke:#b45309,color:#111;
  classDef pod fill:#bfdbfe,stroke:#1e40af,color:#111;
  class EXT remote;
  class GW,CORE,AGENT pod;
```

Only the client (extension) is remote; core, gateway and agent are one in-pod `openab run` process
tree. The downstream hop has **one** delivery path: the OAB MCP Facade. It had two others — `proxy`
(HTTP MCP, once the default) and `bridge` (stdio relay) — and both were removed on 2026-07-28.

## 5. MCP usage sequence (katashiro.click as the example)

```mermaid
sequenceDiagram
    autonumber
    participant Tab as Chrome tab<br/>(user's real, logged-in)
    participant Ext as browser ext.<br/>MCP SERVER
    participant GW as openab-gateway<br/>/acp WS
    participant Core as openab-core<br/>OAB MCP Facade
    participant LLM as agent LLM<br/>MCP client

    Note over Ext,LLM: PHASE 1 — connect & agent wiring (no tool discovery yet)
    Ext->>GW: WS GET /acp — initialize<br/>mcpServers = [ type:acp, name:"katashiro" ]
    GW-->>Ext: initialize result (agentCapabilities)
    Ext->>GW: session/new  (or session/resume on reconnect)
    GW->>GW: register per-session TunnelHandle<br/>(AcpTunnelRegistry)
    GW->>Core: spawn agent (mint facade session token)
    Core->>Core: author .openab/mcp-facade.json (the ONE file openab owns)<br/>{url, Authorization: Bearer ${OPENAB_SESSION_TOKEN}}<br/>operator puts the entry in front of their agent
    LLM->>Core: MCP initialize + tools/list
    Core-->>LLM: the facade's TWO meta-tools ONLY<br/>(search_capabilities · execute_capability) — returns at once,<br/>no upstream call; katashiro.* are NOT in the model's tool list

    Note over Tab,LLM: PHASE 2 — discovery is PULL-triggered by the model
    LLM->>Core: search_capabilities("browser")
    Core->>GW: tools/list  (MCP-over-ACP: mcp/message frame)<br/>spawned on first pull per (channel_id, declared_name), then cached
    GW->>Ext: mcp/message → tools/list
    Ext-->>GW: 5 tools: read_dom · screenshot · navigate · click · type
    GW-->>Core: tools result
    Core-->>LLM: capabilities: openab-browser:katashiro.*

    Note over Tab,LLM: PHASE 3 — one autonomous action (e.g. click)
    LLM->>Core: execute_capability("openab-browser:katashiro.click", {selector})
    Core->>GW: tools/call  (mcp/message over the SAME /acp WS)
    GW->>Ext: mcp/message → tools/call
    Ext->>Tab: chrome.scripting / tabs API<br/>click · type · read_dom · captureVisibleTab · navigate
    Tab-->>Ext: DOM mutated / navigated / pixels
    Ext-->>GW: tool result<br/>(screenshot = JPEG q70, frame <= 8 MiB)
    GW-->>Core: result
    Core-->>LLM: tool result
    LLM->>GW: session/update agent_message_chunk (narration)
    GW->>Ext: streamed to the side panel

    Note over GW,Ext: only the gateway-to-extension hop leaves the pod. LLM, core and gateway stay in-pod.
```

The exact two-id-space bookkeeping (outer ACP-envelope id ↔ inner MCP id, flattened per the RFD) is
detailed in **§7.3**.

## 6. Generalization — multiple client-side MCP servers

The browser path wires **one** MCP server. This section is the accepted direction, **as-built in
#1447**, making reverse MCP-over-ACP **generic**: any ACP WS client may declare **one or more**
`type:acp` MCP servers on `session/new` (and re-declare them on `session/resume`) — **not** on
`initialize`, which never reads `mcpServers` — and the agent's LLM discovers and calls each server's real
tools. The browser extension becomes *one instance* of the mechanism, not a special case.

Three pieces already generalize and are reused as-is:
- `parse_acp_mcp_servers` already parses **N** `type:acp` entries with arbitrary `{id, name}`.
- `establish_and_register_tunnel(…, srv.id, …)` already threads the declared `srv.id` into
  `mcp/connect` — the wire already carries a per-server discriminator.
- ~~`ProxyHandler::forward_tool_call` forwards **any** tool name+args down the tunnel — no
  browser-specific validation.~~ `ProxyHandler` was removed with the per-session proxy on
  2026-07-28. Forwarding is now `AcpTunnelSource::call` in the facade capability source, and it is
  **not** unvalidated: the §6.4 trust gate refuses a tool whose server name is not allowlisted or
  whose name is not pinned, before anything reaches the tunnel.

### 6.1 Address every hop by `(channel_id, serverId)`
- `AcpTunnelRegistry` becomes keyed by `(channel_id, serverId)` instead of `channel_id` alone — the
  "one tunnel per session" collapse was a fan-out fix; the correct fix is a **compound key**.
- Rename the core trait `BrowserTunnel` → **`AcpMcpTunnel`**; `call(channel_id, server_id, method, params)`.
- On session teardown, evict only the `(channel_id, *)` entries **this connection owns** —
  matched on `owner`, not on the channel. Evicting every entry for the channel would delete a
  successor's live tunnel, because a client that reconnects and resumes takes over the same
  `channel_id`. (The unqualified form was this document's original wording and describes a
  defect that was fixed in the implementation; left uncorrected it is the copy that could get
  the code "restored" back into the bug.)

**`id` and `name` are different things, and routing needs both.** A declaration is
`{type:"acp", id, name}`, and the two fields have very different lifetimes — the reference client mints
`id` as a fresh `crypto.randomUUID()` **per connection** while `name` (`"katashiro"`) is stable
across reconnects. The registry key is the **`id`**; the `<server>` segment of a tool name (`katashiro.click`)
and the §6.4 allowlist are the **`name`**. Consequences, all confirmed by review 2026-07-26:

- The registry stays keyed by `(channel_id, id)` — keying by `name` would let two same-name tunnels
  overwrite each other, reintroducing exactly the fan-out collapse this section fixes — but it must
  **also record the declared `name`**, so a source can enumerate `(name, id)` for a channel and resolve
  a tool prefix to a tunnel. Routing purely on the registry key cannot work: the key is a UUID the tool
  name never contains.
- Trust gating (§6.4) is keyed by **`name`**. An allowlist of `id`s is meaningless when they are
  per-connection UUIDs.
- **Same-name collisions resolve last-attach-wins (LWW):** a newly attached tunnel whose `name` matches
  an existing one on the same channel **replaces and evicts** the older entry. Because the client mints
  a new `id` on every reconnect, the stale entry would otherwise linger beside the live one; answering
  "ambiguous, disambiguate by server_id" there would wedge the client out of its own tools on every
  reconnect. LWW keeps reconnect self-healing; the eviction is what stops unbounded growth.

### 6.2 Downstream exposure — one `CapabilitySource` behind the OAB MCP Facade

> **Revised 2026-07-26.** An earlier draft of §6.2–§6.5 proposed a bespoke path: per-`(session, server)`
> loopback MCP proxies, openab writing N entries into the agent's MCP config, dynamic `tools/list` with
> `notifications/tools/list_changed`. That is **superseded**. The
> [OAB MCP Facade](../oab-mcp-facade.md) ([OAB MCP Adapter ADR](./oab-mcp-adapter.md), #1446; facade
> #1448/#1453) and its **session-aware in-process capability sources** (#1454) already provide the
> multi-provider catalog, discovery, policy runtime (schema validation, timeouts, circuit breaking,
> redaction, audit) and lifecycle this section was about to reinvent. Reverse-MCP-over-ACP contributes
> the one thing the facade lacks: a **transport for providers that cannot listen and are dialled in by
> the client**. As of 2026-07-26 the whole facade series is merged upstream (#1446/#1448/#1449/#1450/
> #1453/#1454) and no facade PR remains open, so this section builds on a settled foundation.
>
> The adapter ADR reaches the same conclusion from the other side: its §6.2 states that the facade
> occupies "the same architectural role" this ADR assigns to OpenAB
> core… browser tools and external capabilities **share the delivery mechanism**", and its Alternative C
> rejects "a second generic inbound MCP server", i.e. **no agent-facing MCP server beyond this one
> aggregation point**. That makes retiring the bespoke per-session proxy (F5) a requirement of the
> upstream design, not merely cleanup.

```
Facade providers today:   stdio(command)   http(url)            ← openab dials OUT
Reverse-MCP adds:         acp-tunnel(channel_id, server_id)     ← client dialled IN, openab tunnels
```

**Decision: expose every client-declared `type:acp` server through a single in-process
`CapabilitySource` — `AcpTunnelSource` — registered once with the facade.**

- The seam is `openab-mcp`'s `CapabilitySource` (`provider()` / `tools(ctx)` / `call(ctx, tool, args)` /
  `requires_session()`), with `SessionCtx { channel_id }` identifying the owning chat session. This is
  precisely the case #1454 was built for ("browser control, where `browser.click` must reach *that
  conversation's* browser tab").
- `AcpTunnelSource` lives in the **root binary**, where the tunnel state (`AcpTunnelRegistry`) already
  lives — keeping `openab-core` and `openab-gateway` sibling-independent, as with `RootAcpTunnel`.
- `requires_session() == true`: anonymous facade clients neither discover nor can execute these tools.
- **One source, N servers.** Facade sources are registered **once at construction**
  (`facade::serve_http_with(addr, sources, tokens)`; there is no runtime registration API), so a source
  *per* client-declared server is not possible — and not needed. `AcpTunnelSource` fans out internally:
  `tools(ctx)` returns the tools of **every** `type:acp` server declared by the client of that
  `channel_id`, and `call` routes on the **`<server>.<tool>`** prefix to the matching tunnel. Today's
  names (`katashiro.click`, `katashiro.read_dom`) already carry the server segment, so this generalizes
  with no renaming — but note the segment is the declared **`name`**, not the registry key: resolving
  it to a tunnel goes `name` → `(channel_id, id)` via the recorded declaration (§6.1), never straight
  to the key. The tool name forwarded over the tunnel stays the **full** name the server published
  (`katashiro.click`), since that is what the server's own `tools/call` expects; the prefix selects the
  tunnel, it is not stripped. The facade additionally publishes a `<provider>:<tool>` form to resolve
  shadowing against `mcp.json` servers.
- **Adding another client-side MCP service is therefore declaration + policy work, not architecture
  work.** The source must contain no browser-specific branch.

**Session identity** is the facade's `SessionTokens`: the broker mints one opaque bearer per agent
session, injects it into that agent's process environment as `OPENAB_SESSION_TOKEN`, and revokes it
on session evict. The config file gets only the literal `${OPENAB_SESSION_TOKEN}` reference, never the
value — that is what keeps the secret out of a shared workdir; the facade resolves the header back to a `SessionCtx` per request. This **replaces** the
bespoke per-session loopback proxy, its self-minted port/bearer, and openab's own `openab-browser`
`mcp.json` write/strip logic.

### 6.3 Tool discovery — fetch once per declared server, then serve from cache

The facade's discovery is **pull-based**: the agent sees only `search_capabilities` /
`execute_capability` and re-reads the catalog on each call. Two consequences:

- **`notifications/tools/list_changed` is dropped.** There is no cached client-side tool list to
  invalidate, so the notification has no consumer. (The earlier draft's `list_changed` lifecycle,
  debouncing included, is removed rather than deferred.)
- **A catalog that does not flap is the right posture**, per the facade's source contract —
  implemented as *dynamically sourced, then cached*, because tools for arbitrary declared servers
  cannot be hardcoded. (This bullet said "static-advertise is the right posture" until D-20 deleted
  the built-in catalog; advertising before a server has spoken is no longer possible, so the posture
  is discover-then-hold.):
  fetch the server's real `tools/list` over its tunnel and **cache it per `(channel_id, name)`**;
  serve `tools(ctx)` from that cache **regardless of current attach
  state**. Backend unavailability surfaces as a **call error** ("browser not connected"), never as a
  vanishing catalog entry.

Distinguish two kinds of variation: **session scope** is legitimate; **attachment flapping** (is the
tab connected this second) must not reach the catalog. Note what `tools(ctx)` actually varies by
session is the **discovery cache**, not the declaration set — it iterates the *operator policy* map,
so an allowlisted server appears in the catalog independently of what the client declared, and a
client-declared server with no policy entry contributes nothing. ~~A pinned server is advertised even
when the client declared nothing~~ — that no longer follows: with the seed deleted (D-20) an
allowlisted server contributes nothing until its own `tools/list` has been fetched. An optional refinement, requiring a client wire change, is to
carry a tool manifest in the `session/new` declaration so the catalog is known without a round-trip.

**Two layers, and the policy table is the lower one** (confirmed by review 2026-07-26; an earlier draft
of this section said an un-cached server "contributes an empty set", which contradicted the
static-advertise posture §6.4's pinned sets and D4 both depended on — and which, after D-20 deleted
the seed, is now exactly what happens):

- ~~The §6.4 policy entry for a server is its **pre-attach seed** as well as its filter. A server the
  operator has pinned advertises those tools from the moment the source is registered — it never drops
  to empty just because nothing has attached yet. This is what preserves D4's "the browser tools are
  discoverable before the extension connects".~~ **The seed mechanism was removed on 2026-07-30
  (D-20)**: `policy_from_config` now sets `seed: Vec::new()` for every entry, so a policy entry is a
  filter and nothing else. D4's "discoverable before the extension connects" no longer holds — there
  is a cold-start window in which a configured server advertises nothing until its first `tools/list`
  returns. The strikethroughs at `:334` and `:421` retire the *katashiro five tools* claim; this one
  retires the *mechanism*, which outlived it.
- The per-`(channel_id, name)` cache holds what the server published and is read as
  `fetched ∩ allowed`, **replacing the seed once a fetch succeeds**, so the catalog narrows to what the
  server actually publishes (a server may publish fewer tools than the operator permitted) without ever
  widening past the policy. Filtering on read rather than on write means tightening the policy takes
  effect immediately instead of waiting for a cache entry to be invalidated — **caching is never itself
  a grant**.
- **The cache is keyed by the declared `name`, not `server_id`** (corrected 2026-07-26; earlier drafts
  of this section said `server_id`). Ids are minted per connection, so an id-keyed entry would be
  orphaned by exactly the reconnect the cache exists to survive — it could never outlive the attach it
  was populated from, which is the opposite of "serve regardless of current attach state". Same-name
  collisions are impossible by §6.1's last-attach-wins rule, so the name is a safe key.
- Discovery is **pull-triggered**: a declared server with no cache entry has its fetch started from the
  next `tools(ctx)` call, and its real set appears one discovery round later. The facade re-reads the
  catalog on every call, so a single round of staleness is the entire cost, and it avoids threading an
  attach hook from the gateway (which owns attach) into the root (which owns the source).
- A declared server with **no** policy entry contributes nothing — not because it is un-cached, but
  because §6.4 is deny-all. Caching changes what an *allowed* server advertises; it is never itself a
  grant.

**Ordering consequence (as reasoned at the time).** ~~Because the filter is deny-all and pinned
entries already carry full `Tool` schemas, fetching cannot surface anything an operator has not
already permitted — so the discovery cache has no visible effect until the operator-facing
configuration surface exists.~~ The config surface landed **first**, which was the decision this
paragraph drove.

Both halves of the premise are now false: entries carry no schemas (D-20 deleted the catalog) and the
configuration surface has shipped. The conclusion inverted with them — the discovery cache is not
invisible but **load-bearing**, because it is the only source of schemas. This is also the argument
retracted in the status comment: "discovery is unnecessary because the schemas are hardcoded" rested
on the seed that was deleted.

### 6.4 Trust — client-declared tool sets need an operator gate

#1454 states that source registration *is* the operator's grant, and that sources therefore carry no
per-source `tool_filter`. That assumption holds for code-wired sources whose tool set the operator
chose. It **does not hold** for `AcpTunnelSource`, whose tool set is declared by a **remote client**: a
connected extension could otherwise publish arbitrary tools into the agent's capability catalog.

Therefore this ADR requires, before the source is enabled by default:

- an operator **allowlist** of accepted declared server names (~~default: `katashiro` only~~ — as of
  2026-07-30 there is NO default; empty admits nothing, D-20) — a
  declaration outside it is refused by the capability source: it contributes no tools and its calls
  return an error result. **Note it is not refused at declaration time** — the gateway still opens
  and registers the tunnel — **and nothing is logged today** (see the gap noted below); and
- a per-declared-server **`tool_filter`**, mirroring `mcp.json` least-privilege semantics, which is
  **deny-all by default**.

> ⚠️ **Two gaps between this section and the code, recorded rather than quietly reworded.**
>
> **No logging.** This section said twice that a refused declaration and a dropped tool are "logged".
> `src/acp_tunnel_source.rs` contains no logging call at all — both refusals are silent. An operator
> who mis-types a server name in `[[mcp.acp_servers]]` gets missing tools and no signal, which is
> the shape of failure this ADR spends §6.4 preventing. **Fixing this is a code change, so it is
> filed as follow-up F7 rather than made here.**
>
> **The policy runtime does not wrap in-process sources.** §6 claims the facade already provides
> "schema validation, timeouts, circuit breaking, redaction, audit" to capability sources. Only
> argument validation and audit apply on the source path (`facade.rs` `execute_capability`);
> timeout/cancellation, the circuit breaker and redaction live in `meta_tool::dispatch`, which only
> downstream `mcp.json` servers traverse. A hung browser tunnel is bounded by the tunnel's own
> timeout, not by the facade's. **Also F7.**

The name allowlist is **not** a trust boundary on its own: the name is chosen by the same remote
client that declares the tools, so a client may declare a server under an allowlisted name — say
`katashiro` — and publish any tool set under it. Passing the allowlist therefore grants nothing by itself — the tool set is gated
separately:

- ~~the `katashiro` entry ships **pinned to its five known tools**~~ — the built-in catalog was
  deleted on 2026-07-30 (D-20): schemas now come only from discovery over the tunnel, so a
  configured server publishes nothing until its first `tools/list` returns. Historically it was
  (`katashiro.read_dom`,
  `katashiro.screenshot`, `katashiro.navigate`, `katashiro.click`, `katashiro.type`); any other tool name it
  declares is dropped, so a same-name declaration cannot inject new tools (dropped silently today —
  see the gap below); and
- every other allowlisted server starts **deny-all** and serves only the tools an operator has
  explicitly listed.

### 6.5 Backward compatibility & what this retires

The browser extension is **unchanged**: it declares `{type:acp, id, name}` and serves its five DOM
tools over the tunnel. What changes is on the openab side — browser tools reach the agent through the
facade's meta-tools rather than a dedicated per-session MCP server.

**Retired in this PR (2026-07-28):** the per-session `mcp_proxy` browser server, its port/bearer
minting, and the `openab-browser` `mcp.json` injection — along with the stdio bridge described
below. Both legacy transports are gone; the facade is the only downstream path.

**Update — the operator call was made on 2026-07-28: bridge mode is removed.** The stdio bridge
(`OPENAB_BROWSER_MODE=bridge`, `openab browser-bridge`, the per-pod unix socket and its
process-ancestry channel resolver) existed because some CLIs preferred a stdio entry. The facade is
a loopback HTTP MCP server those CLIs read directly, so the premise no longer held. ~~Facade setup
deletes the leftover static entry, which is the only one whose exact shape proves we wrote it.~~
That deletion was performed by editing the operator's file, and openab stopped doing that on
2026-07-30 (D-15): it authors `.openab/mcp-facade.json` and touches nothing else. **Removing a
leftover bridge entry is now the operator's step, and it is a policy question rather than tidiness
— while it is present there is a route to the browser that bypasses facade policy and audit.** The
`jq` snippet in `docs/browser-mcp-agent-setup.md` covers it, including the kiro agent-file
`@openab-browser` grant.

This paragraph said `bridge` "degrades to `facade`", which was true for one commit. The per-session
proxy was removed hours later, taking `BrowserMode` and the whole `OPENAB_BROWSER_MODE` mechanism
with it. The variable was deleted outright on 2026-07-31 (D-23), once it was verified never to have shipped outside this branch — not in `origin/main` and not in any release — so it is neither read nor reported, and nobody upgrading can have it set.

**Open question (not decided).** Under the facade the LLM reaches a browser action via
`search_capabilities` → `execute_capability`, one hop more per turn than today's direct
`katashiro.click`. Recommendation: ship on the meta-tool path (uniform policy, one audit surface) and
revisit a per-provider "expose directly" option only if interactive browser latency proves it needed.

### 6.6 Status — as-built vs remaining

**As-built (`bf37d25e`, `74e23f0e`): the facade is the only transport.** `src/acp_tunnel_source.rs`
(renamed from `browser_source.rs`) implements `CapabilitySource` over the existing `AcpMcpTunnel` —
`requires_session()`, a catalog that does not shrink on detach per §6.3, tunnel failures surfaced as
MCP error results — and a `FacadeRegistrar` adapts the facade's
`SessionTokens` to a `SessionTokenRegistrar` hook in core, so `openab-core` stays free of an
`openab-mcp` dependency. `write_facade_mcp_config` authors `.openab/mcp-facade.json` — the one file openab owns — containing
a **static `openab` entry** whose
`Authorization` references `${OPENAB_SESSION_TOKEN}`, so the per-session secret rides the agent's
process environment rather than a config file — which also removes the shared-workdir exposure of the
old per-session `mcp.json` write. Capabilities publish under the provider name `openab-browser` (`openab` is the key of the entry
inside `.openab/mcp-facade.json` — a different thing, and no longer a key openab writes into anyone
else's `mcp.json`). Both
legacy transports were removed on 2026-07-28 — bridge first, then the per-session proxy — and
`OPENAB_BROWSER_MODE` no longer selects anything; `[mcp]` is now required for browser control.
This covers §6.2's source seam and session identity for the **browser** case.

⚠️ **Divergence to reconcile with the adapter ADR (not resolved here).** Adapter ADR §6.2 says delivery
is via ACP `session/new` `mcpServers`, and that "if a backing CLI does not honor ACP `mcpServers`, the
facade is unavailable for that CLI in the MVP **rather than falling back to editing the CLI's config
files**". ~~The as-built `write_facade_mcp_config` does write a static entry into the CLI's config —
deliberately, because the browser path's D2 established that Cursor ignores ACP-passed `mcpServers`
(**§7.2** D2, [zed#50924](https://github.com/zed-industries/zed/issues/50924)).~~

That premise is struck rather than rewritten, because it is the *statement of what the divergence
was*: editing it into present truth would leave a resolution with nothing to resolve. As of
`30e04758`, `write_facade_mcp_config` authors `.openab/mcp-facade.json` and edits no CLI config at
all, which is what removes the conflict — see the resolution note immediately below.
~~Both positions are defensible; recording the conflict rather than silently picking a side. Owner of the
facade contract should confirm whether config-file injection is an accepted exception for CLIs that
ignore `mcpServers`, or whether Facade mode should be unavailable for them.~~

**RESOLVED 2026-07-30 (D-15), in favour of the adapter ADR.** openab does not edit a CLI's config
files, and does not invoke a vendor CLI to do it either. It authors `.openab/mcp-facade.json`; the
operator puts that entry in place (`kiro-cli mcp import --file … workspace` for kiro, by hand for
cursor, which has no include/extends and no launch flag). The cost is stated rather than hidden:
kiro and cursor both lose zero-config onboarding, which is wider than the cursor-only regression
first recorded. Whether Claude Code is pointed at the file with `--mcp-config` at spawn was SETTLED on
2026-07-31 (D-21, shipped in `54223aea`): openab does not pass it. `[agent]` is an opaque
command line spawned verbatim, so the operator adds the flag to `args` themselves. Having
openab identify the vendor at spawn time was rejected — this codebase negotiates capability
from the protocol and deliberately has no vendor-identity concept.

**Remaining to fulfil this section** — F1′, F3′, F4 and F5 all landed in #1447 and are struck
through; **F6 genuinely remains**:
- ~~**F1′ generalize the source to N client-declared servers.**~~ **Done in #1447**: the source
  holds an N-entry policy map and routes on the `<server>.<tool>` prefix, resolving the declared
  name to its tunnel. It no longer *enumerates* — `tunnel.servers(channel_id)` was deleted because
  enumerate-and-match was the wrong route (`74315a60`), and `builtin_catalogs` was deleted by D-20,
  so browser-ness is not data here either: openab holds no catalog at all. What makes the source
  generic is that it knows only names the operator listed.
- ~~**F3′ per-`(channel_id, name)` discovery cache**~~ **Done in #1447**: `ToolsCache` keyed
  `(channel_id, declared_name)` with in-flight dedupe and pull-triggered discovery (§6.3).
- ~~**F4 trust gate** — operator allowlist + **deny-all-by-default** per-declared-server
  `tool_filter` (§6.4).~~ **Done in #1447**: `ServerPolicy` / `policy_from_config` over
  `[[mcp.acp_servers]]`, enforced in both `tools()` and `call()` before the tunnel is resolved;
  ~~default allowlist is `katashiro` pinned to its five tools~~ — there is no default allowlist as
  of 2026-07-30 (D-20).
- ~~**F5 cleanup** — retire the superseded per-session proxy path once Facade mode has soaked;
  bridge-mode removal stays an explicit operator call (§6.5).~~ **Done 2026-07-28**: the operator
  call was made and both transports were removed in this PR, so there is no soak period and no
  remaining opt-out.
- **F7 close the two §6.4 gaps** — (a) log a warning when a declared server is refused by the
  allowlist and when a fetched tool is dropped by the pin, since both are silent today and an
  operator's only symptom is missing tools; (b) decide whether in-process capability sources should
  traverse the same timeout / circuit-breaker / redaction path as downstream servers, or whether the
  ADR should stop claiming they do. Both are code changes, deliberately not made in #1447.
- **F6 e2e** — browser + a second client-declared server + a host-level `mcp.json` provider coexisting,
  and two concurrent sessions each reaching only their own browser.

## 7. Worked example — browser control

The driving consumer of this mechanism, and the design the **browser extension** implements. The
wire contract it codes against is [`mcp-over-acp-tunnel-contract.md`](../mcp-over-acp-tunnel-contract.md);
how the agent is wired to reach the tools is [`browser-mcp-agent-setup.md`](../browser-mcp-agent-setup.md).

### 7.1 Toolset

Five **DOM-semantic** MCP tools, served by the extension: `katashiro.read_dom` (snapshot),
`katashiro.screenshot`, `katashiro.navigate`, `katashiro.click(selector)`,
`katashiro.type(selector, text)`.

- **DOM-semantic, not a model-specific `computer` (pixel) tool** — `click(selector)` / `read_dom`
  are cheaper, more reliable, and model-agnostic; screenshot + coordinates remain expressible if
  wanted, but are not the primary surface.
- **Screenshots are JPEG** (`captureVisibleTab {format:"jpeg", quality:70}`, ~300–500 KB); the ACP
  frame cap is raised 1→8 MiB to carry tool results. PNG base64 (~5.5 MB) exceeded the **old** 1 MiB
  cap, which is why JPEG was chosen; it fits within the 8 MiB cap that replaced it.
- The declared server name is `katashiro`; it was `browser` until 2026-07-26, when it was renamed
  because Playwright MCP's `browser_*` tools sat beside it in the same catalog and the model could
  not reliably tell "the user's real logged-in tab" from "a sandbox browser".

### 7.2 Design decisions (D1–D6)

> **Supersession notice.** D2, D3 and D5 record the **original** delivery path: a per-`acp:`-session
> loopback MCP proxy registered in each agent's native MCP config. That path is superseded by the
> facade integration in §6.2 — browser is now one session-aware `CapabilitySource`, and session
> identity is the facade's broker-minted `SessionTokens` rather than a per-session port plus a
> self-written `mcp.json` entry. They are kept because they explain *why* the shipped design looked
> the way it did. Neither `proxy` nor `bridge` is selectable any more — both were removed on
> 2026-07-28. D1, D4 and D6 carry over.

- **D1 — permission model.** Auto-approve **all** browser tool permissions for now: core keeps
  auto-replying `session/request_permission` with OK. Fine-grained consent is deferred. Consequence:
  a dedicated `request_permission`-relay task is **dropped**, but the server→client request machinery
  is still required for the upstream MCP tunnel. (That direction was not green-field: `openab-core`'s
  ACP connection already received `session/request_permission` from the agent and auto-replied it, so
  the work was *relaying* those upstream rather than inventing the path.)
- **D2 — how the agent receives the tools (injection).** The ACP `session/new` `mcpServers` parameter
  is **not** reliable: Cursor's CLI ignores ACP-passed MCP servers and only loads MCP from its **own
  config** (`.cursor/mcp.json`) — see [zed#50924](https://github.com/zed-industries/zed/issues/50924).
  So the server is registered **per-agent, in that agent's native MCP config** (Cursor →
  `.cursor/mcp.json`; Kiro → `.kiro/settings/mcp.json`). The **content** (an HTTP MCP entry: `url` +
  `headers`) is portable across vendors. Under §6.2 this became a *static* entry referencing
  `${OPENAB_SESSION_TOKEN}` instead of a freshly minted per-session URL.
- **D3 — where MCP is tunnelled.** Downstream (agent ↔ core) is a **normal** in-process
  Streamable-HTTP MCP server on `127.0.0.1:<port>` (loopback + bearer, via `rmcp`); the agent connects
  to it like any other MCP server. Only the **upstream** (core/gateway ↔ extension) is tunnelled — an
  MV3 extension cannot listen — adopting the official
  [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) framing (`mcp/connect` →
  `connectionId`, then `mcp/message`), not a hand-rolled envelope.
- **D4 — lifecycle: the WS may connect *after* session start.** When `[mcp]` is configured the
  facade listener is process-lifetime and decoupled from the extension WS — it is not
  unconditionally always-on, since without `[mcp]` no listener starts at all and there is no browser
  control. Given a listener, an allowlisted server's tools stay in the catalog regardless of WS
  state once discovered; a `tools/call` with no extension attached returns an MCP error ("browser
  not connected") rather than the capability disappearing. ~~Tools are **static-advertised**
  regardless of WS state~~ — before discovery has run there is nothing to advertise (D-20). `notifications/tools/list_changed` was designed but never implemented,
  and is **dropped, not deferred** (§6.3): facade discovery is pull-based, so no cached tool list
  exists for a notification to invalidate. ~~The static-advertise posture is kept~~ — it is not; what
  is kept is that a discovered catalog does not shrink, implemented as
  fetch-once-per-declared-server plus a per-`(channel_id, declared_name)` cache — keyed by NAME, not
  `server_id`, so a reconnect that mints a fresh id does not lose the cache (§6.3).
- **D5 — per-session MCP server.** The pool started one loopback Streamable-HTTP MCP proxy per `acp:`
  session at agent spawn, constructing the `ProxyHandler` with that session's `channel_id` so
  correlation was implicit; lifetime was tied to the `AcpConnection` via a `CancellationToken`
  `DropGuard`. Superseded by the single facade listener (§6.2). `proxy` mode kept this behaviour
  until it too was removed on 2026-07-28, so this section is now purely historical.
- **D6 — tunnel trait in core, impl in root.** `openab-core` defines the tunnel trait (`AcpMcpTunnel`,
  §6.1); the **root** binary implements it (`src/acp_tunnel.rs`) by looking up the gateway's
  `AcpTunnelRegistry` and calling `TunnelHandle::mcp_message`. This keeps `openab-core` and
  `openab-gateway` sibling-independent (no cross-crate dep), mirroring the `ChatAdapter` root-glue
  pattern, and is why the `CapabilitySource` in §6.2 also lives in the root binary.

### 7.3 Runtime detail — one `katashiro.click` round-trip, and the two id spaces

§5 gives the phase-level view; this is the message-level detail. Transports below are `proxy`-mode
(agent ↔ core over loopback HTTP); under facade mode that hop is the facade listener instead, and the
id bookkeeping is unchanged.

```
Participants  A = agent/LLM (Cursor, MCP client)   C = core (in-pod MCP server + proxy)
              G = gateway (/acp WS srv)             E = extension (MCP server, browser)

Transports    --ACP-->  downstream ACP over stdio (chat / permission)
              --HTTP--> downstream HTTP MCP, 127.0.0.1 loopback (tools)
              ==WS===>  upstream /acp WebSocket (official mcp/message tunnel; only hop off-pod)

Precondition: session open, extension WS attached, tools already discovered
--------------------------------------------------------------------------------
 1  A --ACP-->  C   session/request_permission {toolCall:"click #submit"}    id=acp#1
 2  A <--ACP--  C   result: allow               <- core auto-approves (D1)   id=acp#1
 ..............................................................................
 3  A --HTTP--> C   tools/call name=katashiro.click args={selector:"#submit"}  id=mcp#7
 4  C --(in-pod handoff)--> G   wrap upstream: mcp/message  connId=conn-1
                                 params={method:"tools/call", ...} FLATTENED, no inner id   id=acp#55
 5  G ==WS===>  E   server->client request = MCP-over-ACP             outer id=acp#55  <-off-pod
 6            E     chrome.scripting.executeScript -> clicks #submit, page -> /thanks
 7  G <==WS==  E    response result={ok,url:"/thanks"} (the inner MCP result)   outer id=acp#55 <-on-pod
 8  C <--(in-pod)-- G   gateway pending-map matches acp#55 -> core maps the result back to mcp#7
 9  A <--HTTP- C    tools/call result {content:[{text:"clicked; now /thanks"}]}  id=mcp#7
 ..............................................................................
10  A              LLM consumes the tool result, keeps reasoning
11  A --ACP-->  C   session/update agent_message_chunk {"I clicked Submit..."}   (notif)
12  C ==WS===>  E   chat stream forwarded on /acp -> user sees narration        <-off-pod
--------------------------------------------------------------------------------
Two id spaces (never mixed)
  - mcp#7  = MCP-layer id, lives ONLY on the agent<->core hop (steps 3/9). Per the RFD,
             mcp/message FLATTENS the inner method/params and does NOT carry an inner MCP id, so
             mcp#7 never travels on the tunnel.
  - acp#55 = outer ACP-envelope id correlating the whole upstream round-trip (steps 4<->8); the
             response result IS the inner MCP result payload. `AcpTunnelSource::call` in the facade
             capability source maps mcp#7 <-> acp#55.

             This said "the proxy maps" until 2026-07-31. The per-session proxy was removed on
             2026-07-28 and this is §7.3 Runtime detail — a description of how the CURRENT
             round-trip works, not history — so the sentence named a component that no longer
             exists. The hop in step 4 is the facade's in-process capability source; the shape of
             the diagram is unchanged, only what performs the hop.
  - acp#1  = downstream ACP permission id; unrelated to the two above

Only steps 5/7/12 leave the pod (all on the /acp WS).
```

### 7.4 As-built history

The OpenAB side was wired end-to-end on 2026-07-20 and live-validated on a real deployment: the full
loop (read_dom / screenshot / navigate / click / type), the side-panel status pill, and reconnect on
`session/resume`. At that point the realised path was
`agent → core per-session ProxyHandler → tunnel trait → root impl → AcpTunnelRegistry → extension`,
with per-agent config injection. `bridge` mode (stdio relay, Option C) shipped alongside and was
removed on 2026-07-28.

The facade integration in §6 replaced the per-session proxy as the default on 2026-07-25/26 and was
live-validated the same way: with `[mcp]` enabled, `search_capabilities` returns provider
`openab-browser` carrying exactly the pinned `katashiro.*` capabilities, while anonymous facade
clients see only the two meta-tools.

## 8. Alternatives considered

- **Custom `ExtRequest` per action** — rejected: not surfaced to the LLM as a tool, so the model
  can't call it autonomously. Fits OpenAB-driven ops only.
- **Client hosts a standalone MCP server (HTTP/SSE)** — rejected for can't-listen clients: an MV3
  extension cannot open a listening socket.
- **On-stream MCP-over-ACP for the downstream hop** — rejected: agents already connect to normal MCP
  servers well; a special on-stream MCP type is invasive
  ([ACP discussion #58](https://github.com/orgs/agentclientprotocol/discussions/58)). Only the
  can't-listen *client* leg is tunnelled; downstream stays a normal in-process MCP server.
- ~~**Static-advertise as the default** — superseded by §6.2 (dynamic + `list_changed`); kept as an
  opt-in for browser only.~~ **Reversed (2026-07-26):** static-advertise IS the implemented posture,
  `list_changed` was dropped with no consumer (§6.3), and there is no opt-in — the source is
  registered unconditionally whenever `[mcp]` is present. **Reversed again (2026-07-30, D-20):** the
  built-in catalog that made static-advertise possible was deleted, so the posture is now
  discover-then-hold — nothing is advertised before a server's first `tools/list`. The `list_changed`
  and no-opt-in halves still stand.

## 9. Typing / dependencies

- Bidirectional tool-call / client-method messages are where hand-rolling breaks; the expanded
  surface uses **generated** serde-only **v1** wire types (offline `typify` codegen, avoiding the
  `schemars`-heavy `agent-client-protocol-schema` crate). Landed in the base.
- The MCP machinery (handshake, tool lifecycle, tunnel framing) needs an MCP implementation
  (`rmcp`, already used by `openab-agent`) plus the ACP-tunnel transport glue.

## 10. Relationship to Computer Use

Same category as "computer use" (LLM autonomously drives an app via a perceive→act tool loop), but
generalized: (a) targets the **user's real** app/session (e.g. logged-in Chrome), not a sandbox; (b)
the action surface is **client-defined MCP tools** (DOM-semantic or screenshot), not a model-specific
tool; (c) **model-agnostic** — any MCP-capable agent can use it.

## 11. References

- [Base ADR](./acp-server-websocket-base.md) · [Original proposal](./acp-server-websocket.md) ·
  [openab-agent MCP](./openab-agent-mcp.md)
- [MCP-over-ACP tunnel contract](../mcp-over-acp-tunnel-contract.md) ·
  [Browser MCP agent setup](../browser-mcp-agent-setup.md)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) · MCP
  `notifications/tools/list_changed`

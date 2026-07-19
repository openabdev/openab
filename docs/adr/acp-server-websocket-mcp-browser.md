# ADR: Browser control via MCP-over-ACP (proposed)

- **Status:** Proposed (design only — not implemented). North-star capability the base
  builds toward; see the base ADR §6 roadmap "Critical path".
- **Date:** 2026-07-18
- **Author:** @brettchien
- **Related:** [ACP Server over WebSocket — Base (as-built)](./acp-server-websocket-base.md),
  [ACP Server with WebSocket Transport](./acp-server-websocket.md) (original proposal),
  [openab-agent MCP](./openab-agent-mcp.md)

---

## 1. Context

The base ships a 1:1 streaming chat ACP server at `GET /acp`; a browser side-panel
extension connects as an ACP client and drives an OpenAB agent. The next goal is for the
agent's **LLM to autonomously operate the user's browser** (click, read the DOM, navigate)
— i.e. browser "computer use", but targeting the user's real, logged-in Chrome session
rather than a sandbox VM.

## 2. Decision

Expose the browser as **MCP tools** and route them to the agent via **MCP-over-ACP**,
tunnelled over the **existing `/acp` WebSocket** the extension already holds.

Why MCP (not a custom ACP `ExtRequest`): for the LLM to *autonomously* use browser
actions, they must appear in the agent's tool list (`tools/list`) so the model discovers
and calls them. A custom `ExtRequest` is a transport-level ACP extension the LLM never
sees as a tool — it only fits OpenAB-driven (non-LLM) operations. MCP is the standard way
agents receive tools, so browser actions must be MCP tools.

### Roles
- **Extension = MCP server (role/logic).** It handles `tools/list` / `tools/call` and
  executes DOM actions. An MV3 extension cannot open a *listening* socket, but MCP
  server/client is about *who provides tools*, not who opens the connection — so the
  extension serves MCP over the **outbound `/acp` WS it already opened**. This is the only
  way a can't-listen extension can be a full MCP server.
- **OpenAB core = MCP proxy/aggregator.** OpenAB is a middlebox between two ACP
  connections. It consumes the extension's tools from the upstream tunnel and re-exposes
  them to the agent downstream (via `mcpServers`) so the LLM's `tools/list` sees them.
- **Agent = MCP client.** The agent (Claude / Codex / Cursor / Gemini …) is a subprocess
  colocated in the OpenAB pod; it calls the tools over its in-pod ACP/MCP link.

### Call route — the agent is in-pod; only the extension is remote
```
   REMOTE (user's browser)              OPENAB POD  (`openab run` — one process tree)
 ┌──────────────────┐        ┌────────────────────────────────────────────────────┐
 │ browser extension│        │  ┌─────────┐   ┌───────────┐   ┌──────────────────┐ │
 │  = MCP SERVER    │◀─/acp─▶│  │ gateway │──▶│  core     │──▶│  agent CLI       │ │
 │  browser tools   │  WS    │  │ /acp srv│   │ MCP proxy │   │  (subprocess)    │ │
 └──────────────────┘ (only  │  └─────────┘   └───────────┘   │  LLM (MCP client)│ │
                      remote  │                    ▲ in-pod     └──────────────────┘ │
                       hop)   │                    └── stdio: ACP + MCP(mcpServers) ──┘
                             └────────────────────────────────────────────────────┘

 one tool call (LLM clicks a button); only ❸/❺ leave the pod:
  ❶ LLM ─tools/call "browser.click"─▶ core (MCP proxy)      [in-pod]
  ❷ core ─▶ gateway ─❸ MCP-over-ACP──▶ extension            [out of pod → remote]
  ❹ extension runs it in the browser
  ❺ result ──▶ gateway ─❻▶ core ─❼▶ LLM continues           [remote → back in-pod]
```

### One WebSocket, multiplexed
The single `/acp` WS carries BOTH the ACP chat session (initialize / session.prompt /
session.update) AND the tunnelled MCP traffic (tools/list / tools/call / results),
distinguished by ACP method namespace. No second connection.

> **Refinement (see §7 "Design decisions"):** this multiplexing applies to the **upstream**
> hop (extension ↔ gateway), using the official MCP-over-ACP `mcp/message` framing. The
> **downstream** hop (core ↔ agent) is *not* tunnelled over ACP — core hosts a normal
> in-process HTTP MCP server the agent connects to. Only the extension, which cannot open a
> listening socket, needs MCP tunnelled over its `/acp` WS.

## 3. Protocol gap to close first

The base does only client→agent (prompt) and agent→client **notifications** (streaming
text). Browser control needs the **agent→client REQUEST** direction (request/response:
the agent asks the client to do X and awaits a result). The WS is already bidirectional;
`acp_server`'s dispatch loop must add the agent-initiated-request path. This is also the
point to move the wire types from hand-rolled to **generated** (see §5).

## 4. Alternatives considered

- **Custom `ExtRequest` per browser action** — rejected: not surfaced to the LLM as a
  tool, so the model can't autonomously call it. Fits OpenAB-driven ops only.
- **Extension hosts a standalone MCP server (HTTP/SSE)** — rejected: MV3 extensions
  cannot open a listening socket.
- **Anthropic-style `computer` tool (screenshot + pixel coords)** — subsumed: you can
  expose `screenshot` + `click(x,y)` as MCP tools if desired, but DOM-semantic tools
  (`click(selector)`, `read_dom`) are cheaper/more reliable and model-agnostic.

## 5. Typing / dependencies

- Bidirectional tool-call / client-method messages are exactly where hand-rolling breaks;
  adopt **generated types** for the expanded surface. Use **v1** schema (stable; `v2` is
  experimental and currently wire-identical). Prefer offline codegen (e.g. `typify`) to
  emit plain-serde types — this avoids the `schemars`-heavy dependency tree the official
  `agent-client-protocol-schema` crate pulls in for `JsonSchema` derives OpenAB doesn't
  use at runtime.
- The MCP protocol machinery itself (handshake, tool lifecycle, tunnel framing) is NOT
  just types — it needs an MCP implementation (e.g. `rmcp`, already used by
  `openab-agent`), plus the ACP-tunnel transport glue.

## 6. Relationship to Computer Use

Same category as browser "computer use" (LLM autonomously drives a browser via a
perceive→act tool loop), but generalized: (a) targets the **user's real Chrome** (live,
logged-in), not a sandbox; (b) action surface is **extension-defined MCP tools**
(DOM-semantic or screenshot), not a model-specific tool; (c) **model-agnostic** — any
MCP-capable agent can use it.

## 7. Implementation blueprint (task breakdown)

North-star = the agent's LLM autonomously operating the user's browser via MCP tools
tunnelled over `/acp`. The base (PR that revives #1260) ships the 1:1 chat surface and the
**generated v1 wire types** (`acp_schema`, already committed) — one of the four critical-
path items is therefore done. What remains splits cleanly into an **OpenAB (server) side**
and an **extension (client) side**, meeting at a single **MCP-over-ACP wire contract** (T4)
so the two can proceed largely in parallel once that contract is fixed.

### TL;DR — how one browser action flows

```
    [ LLM ]  ────▶  [ OpenAB (core) ]  ────▶  [ browser extension ]
     wants to act      middle-man / relay        operates the real tab
   ──────────────────────────────────────────────────────────────────────
   inside the server pod                          in the user's browser (remote)

  Request  1. LLM decides "click the Submit button"
           2. OpenAB relays the action to the extension in the user's browser
           3. the extension actually clicks it in the active tab
  Result   4. the extension reports "clicked; page went to /thanks"
           5. OpenAB hands the result back to the LLM
           6. the LLM continues → the user sees its narration in the side panel

  The LLM thinks it is calling an ordinary set of tools; in reality OpenAB is a
  middle-man relaying every action to the real remote browser (and relaying the
  tool list the other way). Only the OpenAB↔browser leg leaves the server; the
  LLM↔OpenAB legs stay in-pod. The detailed message-level sequence is below.
```

### Design decisions (resolved 2026-07-19)

Four decisions were worked through and locked; they refine §2 and the tasks below.

- **D1 — permission model.** Auto-approve **all** browser tool permissions for now: core
  keeps auto-replying `session/request_permission` with OK (existing
  `connection.rs` behaviour); fine-grained control is deferred. Consequence: a dedicated
  `request_permission`-relay task (was T3) is **dropped**, but T1's server→client request
  machinery is still required — the **upstream MCP tunnel** needs it.

- **D2 — how the agent receives the tools (injection).** The ACP `session/new` `mcpServers`
  parameter is **not** reliable for this: Cursor's CLI ignores ACP-passed MCP servers and
  only loads MCP from its **own config** (`.cursor/mcp.json`) — see
  [zed-industries/zed#50924](https://github.com/zed-industries/zed/issues/50924). So the
  proxy is registered **per-agent, in that agent's native MCP config** (Cursor →
  `.cursor/mcp.json`; others via their own file/format — there is no universal location:
  VS Code uses the `servers` key, Codex uses TOML). The **content** (an HTTP MCP entry:
  `url` + `headers`) is portable across vendors, so "as long as it loads, we're fine".

- **D3 — where MCP is tunnelled.** **Downstream (agent ↔ core) is a *normal* MCP server,
  not an on-ACP-stream tunnel.** The ACP maintainer prototyped on-stream MCP-over-ACP and
  backed off — agents already connect to MCP servers well, and a special on-stream MCP type
  is invasive
  ([discussion #58](https://github.com/orgs/agentclientprotocol/discussions/58)). So core
  hosts a **Streamable-HTTP MCP server in-process** on `127.0.0.1:<port>` (loopback + bearer,
  via `rmcp`); the agent connects to it like any other MCP server. The **upstream**
  (core/gateway ↔ extension) is the one legitimate tunnel (an MV3 extension cannot listen),
  and it adopts the **official [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)**
  framing (`mcp/connect` → `connectionId`, then `mcp/message`), *not* a hand-rolled envelope.
  The RFD's own `"type":"acp"` downstream-injection path is **not** used (Cursor doesn't
  support it; see D2).

- **D4 — lifecycle: the WS may connect *after* session start.** Core's HTTP MCP server is
  **always-on and decoupled** from the extension WS; the extension connecting/disconnecting
  only changes *backend availability*. To let browser tools appear on a session whose WS
  attaches late (reconnect, or attach-to-running-session), core does **both**: (a) **static-
  advertise** the fixed browser toolset regardless of WS state — a `tools/call` while no
  extension is attached returns an MCP error ("browser not connected"), which decouples WS
  timing from session start with no client dependency; **and** (b) emit
  `notifications/tools/list_changed` when the extension attaches/detaches, so agents that
  re-query pick up extension-defined extras and fresh schema.

### Execution flow (as designed)

```
Legend  = ACP (JSON-RPC over stdio)   - HTTP MCP (loopback)   <=> /acp WS (only hop off-pod)
        [C] = MCP client   [S] = MCP server

               OPENAB POD (`openab run`)                         REMOTE (user browser)
 ┌───────────┐   ┌──────────────────────────────────┐         ┌────────────────┐
 │ agent CLI │===│ gateway           core           │         │ browser ext.   │
 │ (Cursor)  │   │ /acp srv     MCP proxy +          │         │ (katashiro)    │
 │ LLM  [C]  │-┐ │            HTTP MCP srv :PORT      │<==WS==> │ [S] browser    │
 └───────────┘ │ └──────────────────────────────────┘         └────────────────┘
               └── http 127.0.0.1:PORT ──┘

Bootstrap
  B1 core starts in-process HTTP MCP server @127.0.0.1:PORT (loopback + bearer)
  B2 core [per-agent adapter] writes the HTTP MCP entry into the agent's native config
     (Cursor → .cursor/mcp.json) BEFORE the agent boots
  B3 extension opens /acp WS <=> gateway; ACP initialize; session/new declares its browser
     MCP server ("type":"acp")
  B4 gateway --mcp/connect--> extension  → connectionId   (upstream tunnel established)
  B5 agent boots, reads config → HTTP-connects to core's MCP server → MCP initialize

Discovery (tools/list)
  D1 agent --http tools/list--> core proxy
  D2 core --mcp/message: tools/list--> gateway <=> extension   (or served from static set, D4)
  D3 extension returns [click, read_dom, navigate, type, screenshot]
  D4 core returns the list --http--> agent  → the LLM now sees browser tools

Runtime (LLM clicks a button)
  1 LLM decides browser.click{selector}
  2 agent ==session/request_permission==> core   → core auto-approves (D1) ==OK==> agent
  3 agent --http tools/call browser.click--> core proxy                    [in-pod]
  4 core --mcp/message: tools/call--> gateway
  5 gateway ==server→client request==> <=> extension                    (leaves pod)
  6 extension runs chrome.scripting click in the active tab
  7 extension ==result==> <=> gateway                                   (back in pod)
  8 gateway → core (match pending id) --http result--> agent  → LLM continues

Only steps 5/7 leave the pod. Outer tunnel ids are paired by the gateway pending-map; the
inner MCP ids are the MCP layer's own bookkeeping and are never inspected by the gateway.
```

### Runtime sequence (detailed) — one `browser.click` round-trip

```
Participants  A = agent/LLM (Cursor, MCP client)   C = core (HTTP MCP srv + proxy)
              G = gateway (/acp WS srv)             E = extension (MCP server, browser)

Transports    --ACP-->  downstream ACP over stdio (chat / permission)
              --HTTP--> downstream HTTP MCP, 127.0.0.1 loopback (tools)
              ==WS===>  upstream /acp WebSocket (official mcp/message tunnel; only hop off-pod)

Precondition: session open, extension WS attached, tools/list already discovered
--------------------------------------------------------------------------------
 1  A --ACP-->  C   session/request_permission {toolCall:"click #submit"}    id=acp#1
 2  A <--ACP--  C   result: allow               <- core auto-approves (D1)   id=acp#1
 ..............................................................................
 3  A --HTTP--> C   tools/call name=browser.click args={selector:"#submit"}  id=mcp#7
 4  C --(in-pod handoff)--> G   wrap upstream: mcp/message  connId=conn-1
                                 params={method:"tools/call", ...} FLATTENED, no inner id   id=acp#55
 5  G ==WS===>  E   server->client request (T1) = MCP-over-ACP       outer id=acp#55  <-off-pod
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
  - mcp#7  = MCP-layer id, lives ONLY on the agent<->core HTTP hop (steps 3/9). Per the
             MCP-over-ACP RFD, mcp/message FLATTENS the inner method/params and does NOT
             carry an inner MCP id, so mcp#7 never travels on the tunnel.
  - acp#55 = outer ACP-envelope id that correlates the whole upstream tunnel round-trip
             (steps 4<->8); the response result IS the inner MCP result payload. The core
             proxy maps its downstream mcp#7 <-> the upstream acp#55.
  - acp#1  = downstream ACP permission id; unrelated to the two above

Only steps 5/7/12 leave the pod (all on the /acp WS). Permission (1-2) and tool transport
(3, 9) stay in-pod on loopback. If the extension is not attached at step 5, core returns an
MCP error "browser not connected" (D4 static-advertise: calls fail gracefully, no crash).
```

### T0 spike checklist (what the live PoC must confirm)

1. Cursor loads an **HTTP** MCP server registered in `.cursor/mcp.json` (auto, or needs a
   one-time `cursor-agent mcp enable`).
2. Cursor honours `notifications/tools/list_changed` and re-fetches mid-session (validates
   D4(b); if not, D4(a) static-advertise carries it).
3. Cursor handles a `tools/call` error ("browser not connected") gracefully.

### Findings that reshape the work
- The **agent→client REQUEST direction already exists on the downstream hop**:
  `openab-core/src/acp/connection.rs` receives `session/request_permission` from the agent
  and currently **auto-replies** it (~L252). So T1 is not green-field — it is *relaying*
  those downstream requests up to the `/acp` client (and the response back) instead of
  auto-answering them.
- `session/new` / `session/resume` currently send `mcpServers: []` (connection.rs L567/784),
  but that path is **not** how the agent gets the browser tools (see D2 — Cursor ignores
  ACP-passed MCP servers). Giving the agent browser tools = core **hosts a proxy HTTP MCP
  server** and registers it in the agent's **native** MCP config (T5); the proxy tunnels
  `tools/*` to the extension over the upstream MCP-over-ACP link.

### Ownership
- **OpenAB side** (`feat/acp-mcp-browser`): T1, T2, T4 (contract + core routing), T5.
- **Extension side** (katashiro): T6; plus the client half of T4 (serve MCP over the
  tunnel). (Permission is auto-approved by core per D1, so no consent UX is needed yet.)
- **Both**: T7.

### Tasks

**T0 — Spike (do first; de-risks everything).** PoC per the **T0 spike checklist** above:
register a mock **HTTP** MCP server in Cursor's `.cursor/mcp.json` and confirm the LLM
discovers (`tools/list`) and calls (`tools/call`) a tool, honours `tools/list_changed`, and
handles a call error gracefully. If this doesn't hold, the browser goal needs a different
path. (The agent→client request direction it depends on already exists downstream — see
Findings.)

**T1 — agent→client REQUEST direction (relay).**
- 1.1 Decide to relay downstream requests (`request_permission`, later MCP) to the `/acp`
  client instead of auto-replying; enumerate the relayed methods.
- 1.2 Gateway outbound request path: `acp_server` sends an agent-initiated REQUEST
  (method + id) to the client and keeps a pending-response map (`id → oneshot`).
- 1.3 Read loop distinguishes an inbound **client response** (`id` + `result`/`error`, no
  `method`) from a client request, and routes responses to the pending map.
- 1.4 core↔gateway bridge: relay the downstream request up + the client's response back
  down to the agent.
- 1.5 Round-trip tests (agent request → client → response → agent).

**T2 — migrate `acp_server` to generated typed wire (bidirectional surface).**
- 2.1 Construct response payloads from `acp_schema` types (the deferred construction
  migration). 2.2 Type the new bidirectional messages (`request_permission`, MCP tunnel).
  2.3 Round-trip validate against real traffic (ACP trace mode).

**T3 — `session/request_permission`.** **Dropped** per D1 (auto-approve; core keeps
auto-replying). Fine-grained permission control is a later, separate effort.

**T4 — MCP-over-ACP tunnel framing (upstream only).** Adopt the official
[MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) rather than a
hand-rolled envelope (D3).
- 4.1 `mcp/connect` (→ `connectionId`) + `mcp/message` (carries the inner MCP JSON-RPC;
  outer ACP id ↔ pending-map, inner MCP id opaque) + `mcp/disconnect`. 4.2 Gateway routes
  these between the `/acp` client (extension) and core. 4.3 Contract doc — done:
  [MCP-over-ACP tunnel — extension implementation contract](../mcp-over-acp-tunnel-contract.md).
  4.4 Mock-MCP-over-tunnel tests.

**T5 — OpenAB core = MCP proxy/aggregator.**
- 5.1 A core-side **Streamable-HTTP MCP server hosted in-process** on `127.0.0.1:<port>`
  (loopback + bearer) that the agent connects to (D3).
- 5.2 **Per-agent adapter** registers that server in the agent's native MCP config (Cursor →
  `.cursor/mcp.json`) before boot, *not* via ACP `session/new mcpServers` (D2).
- 5.3 The proxy acts as an MCP *client* to the extension over the upstream tunnel (T4).
- 5.4 Tool-call routing (agent → proxy → tunnel → extension → result → agent); static-
  advertise the browser toolset + emit `tools/list_changed` on attach/detach (D4).
- 5.5 `rmcp` wiring (already used by `openab-agent`) + tests.

**T6 — extension = MCP server + browser tools** (katashiro).
- 6.1 MCP server role over the outbound `/acp` WS (`tools/list` / `tools/call`).
- 6.2 DOM-semantic tools: `click(selector)` / `read_dom`(snapshot) / `navigate` /
  `screenshot` / `type(selector, text)`. 6.3 Execute in the active tab
  (`chrome.scripting` / content script + permissions). 6.4 Consent UX for
  `request_permission`. 6.5 Tests.

**T7 — integration + e2e + deploy.**
- 7.1 Full loop: `tools/list` → LLM calls `browser.click` → extension executes → result →
  LLM continues. 7.2 A browser-loop e2e (extend `scripts/acp-ws-smoke.py`).
  7.3 Rebuild + redeploy the deployed Cursor agent. 7.4 Finalize this ADR.

### Suggested order
T0 spike → T1 (server→client request direction) → T2 → **T4 (adopt the RFD framing)** → T5
→ then T6 in parallel against the contract → T7. The heavy items are T1 (the direction),
T4/T5 (tunnel + proxy), and T6 (extension). Structured `tool_call` display (base ADR §6) is
parallel and non-blocking.

### As-built (2026-07-20) — OpenAB side wired end-to-end

The OpenAB (server) side is implemented on `feat/acp-mcp-browser` (compiles + unit-tested;
live path pending the extension T6 + deploy T7). Two decisions beyond D1–D4 settled during
implementation:

- **D5 = per-session MCP server.** The pool starts one loopback Streamable-HTTP MCP proxy per
  `acp:` session (in `openab-core/src/acp/pool.rs`, at agent spawn), constructing the
  `ProxyHandler` with that session's `channel_id` so correlation is implicit — it binds to the
  existing `session_key`/`channel_id` map, no in-band id. Server lifetime is tied to the
  `AcpConnection` via a `CancellationToken` `DropGuard`, so it stops on any evict path.
- **D6 = tunnel trait in core, impl in root.** `openab-core` defines
  `mcp_proxy::BrowserTunnel`; the **root** binary implements it (`src/browser_tunnel.rs`)
  by looking up the gateway's `AcpTunnelRegistry` and calling `TunnelHandle::mcp_message`.
  This keeps `openab-core` and `openab-gateway` **sibling-independent** (no cross-crate dep),
  mirroring the existing `ChatAdapter`/`GatewayResponse` root-glue pattern.

Realised call path (all in one `openab run` process):

```
agent tools/call ─http▶ core per-session ProxyHandler (mcp_proxy.rs)
   ─▶ BrowserTunnel (core trait) ─▶ RootBrowserTunnel (root, src/browser_tunnel.rs)
   ─▶ gateway AcpTunnelRegistry[channel_id] ─▶ TunnelHandle::mcp_message
   ═mcp/message═▶ extension    (only this hop leaves the pod)
```

Config injection is per-agent (`.cursor/mcp.json` merged at the session workdir, loopback +
bearer). Static-advertise + not-connected fallback (D4) hold when no browser is attached.
**Remaining:** T5.4 `tools/list_changed` (enhancement; static-advertise already covers the
disconnected case), T6 extension (katashiro — see the
[tunnel contract](../mcp-over-acp-tunnel-contract.md)), and T7 live e2e + deploy.

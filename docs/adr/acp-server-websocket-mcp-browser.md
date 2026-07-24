# ADR: Browser control via MCP-over-ACP

- **Status:** Accepted — OpenAB side **as-built in #1447** (compiles + unit-tested; live-validated
  2026-07-20). The browser extension implements the [tunnel contract](../mcp-over-acp-tunnel-contract.md).
- **Date:** 2026-07-18 (updated 2026-07-24)
- **Author:** @brettchien
- **Related:** **Mechanism — roles, call route, generalization, and the architecture + usage-sequence
  diagrams: [Reverse MCP-over-ACP over WebSocket](./acp-server-websocket-reverse-mcp.md).**
  [Base (as-built)](./acp-server-websocket-base.md), [tunnel contract](../mcp-over-acp-tunnel-contract.md),
  [browser MCP agent setup](../browser-mcp-agent-setup.md).

---

## 1. Context & scope

The [base](./acp-server-websocket-base.md) ships a 1:1 streaming chat ACP server at `GET /acp`; a
browser side-panel extension connects as an ACP client and drives an OpenAB agent. The goal here is
for the agent's **LLM to autonomously operate the user's real, logged-in Chrome** (click, read the
DOM, navigate) — browser "computer use" against the user's own session, not a sandbox VM.

This ADR is the **browser-specific design** and the design the **browser extension** implements. The
underlying transport — how a can't-listen WS client serves MCP over its own `/acp` WS, the roles,
the call route, and the generalization to multiple servers — is the
[Reverse MCP-over-ACP ADR](./acp-server-websocket-reverse-mcp.md); its **§4 architecture diagram**
and **§5 usage-sequence diagram** illustrate this exact browser flow.

## 2. Browser toolset

Five **DOM-semantic** MCP tools, served by the extension: `browser.read_dom` (snapshot),
`browser.screenshot`, `browser.navigate`, `browser.click(selector)`, `browser.type(selector, text)`.

- **DOM-semantic, not a model-specific `computer` (pixel) tool** — `click(selector)` / `read_dom`
  are cheaper, more reliable, and model-agnostic; screenshot + coordinates remain expressible if
  wanted, but are not the primary surface.
- **Screenshots are JPEG** (`captureVisibleTab {format:"jpeg", quality:70}`, ~300–500 KB); the ACP
  frame cap is raised 1→8 MiB to carry tool results. PNG base64 (~5.5 MB) would exceed the cap.

## 3. Design decisions (D1–D6)

- **D1 — permission model.** Auto-approve **all** browser tool permissions for now: core keeps
  auto-replying `session/request_permission` with OK. Fine-grained consent is deferred. Consequence:
  a dedicated `request_permission`-relay task is **dropped**, but the server→client request machinery
  is still required for the upstream MCP tunnel.
- **D2 — how the agent receives the tools (injection).** The ACP `session/new` `mcpServers` parameter
  is **not** reliable: Cursor's CLI ignores ACP-passed MCP servers and only loads MCP from its **own
  config** (`.cursor/mcp.json`) — see [zed#50924](https://github.com/zed-industries/zed/issues/50924).
  So the proxy is registered **per-agent, in that agent's native MCP config** (Cursor → `.cursor/mcp.json`;
  Kiro → `.kiro/settings/mcp.json`; others via their own file/format). The **content** (an HTTP MCP
  entry: `url` + `headers`) is portable across vendors.
- **D3 — where MCP is tunnelled.** Downstream (agent ↔ core) is a **normal** in-process
  Streamable-HTTP MCP server on `127.0.0.1:<port>` (loopback + bearer, via `rmcp`); the agent connects
  to it like any other MCP server. Only the **upstream** (core/gateway ↔ extension) is tunnelled — an
  MV3 extension cannot listen — adopting the official
  [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) framing (`mcp/connect` →
  `connectionId`, then `mcp/message`), not a hand-rolled envelope.
- **D4 — lifecycle: the WS may connect *after* session start.** Core's HTTP MCP server is always-on
  and decoupled from the extension WS. As shipped, browser tools are **static-advertised** regardless
  of WS state (a `tools/call` with no extension attached returns an MCP error "browser not connected"),
  **plus** `notifications/tools/list_changed` on attach/detach. **Superseded as the default:** the
  generic design ([reverse-MCP ADR §6.2](./acp-server-websocket-reverse-mcp.md)) drops static-advertise
  as the default in favour of dynamic `tools/list` forwarding + `list_changed`, keeping static-advertise
  as an opt-in for the browser case.
- **D5 — per-session MCP server.** The pool starts one loopback Streamable-HTTP MCP proxy per `acp:`
  session at agent spawn, constructing the `ProxyHandler` with that session's `channel_id` so
  correlation is implicit. Server lifetime is tied to the `AcpConnection` via a `CancellationToken`
  `DropGuard`, so it stops on any evict path.
- **D6 — tunnel trait in core, impl in root.** `openab-core` defines `mcp_proxy::BrowserTunnel`
  (generically `AcpMcpTunnel` under [reverse-MCP ADR §6.1](./acp-server-websocket-reverse-mcp.md)); the
  **root** binary implements it (`src/browser_tunnel.rs`) by looking up the gateway's
  `AcpTunnelRegistry` and calling `TunnelHandle::mcp_message`. This keeps `openab-core` and
  `openab-gateway` sibling-independent (no cross-crate dep), mirroring the `ChatAdapter` root-glue pattern.

## 4. Runtime sequence (detailed) — one `browser.click` round-trip

The high-level phase diagram is in [reverse-MCP ADR §5](./acp-server-websocket-reverse-mcp.md); this is
the message-level detail, including the **two id spaces**.

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
  - mcp#7  = MCP-layer id, lives ONLY on the agent<->core HTTP hop (steps 3/9). Per the RFD,
             mcp/message FLATTENS the inner method/params and does NOT carry an inner MCP id, so
             mcp#7 never travels on the tunnel.
  - acp#55 = outer ACP-envelope id correlating the whole upstream round-trip (steps 4<->8); the
             response result IS the inner MCP result payload. The core proxy maps mcp#7 <-> acp#55.
  - acp#1  = downstream ACP permission id; unrelated to the two above

Only steps 5/7/12 leave the pod (all on the /acp WS). If the extension is not attached at step 5,
core returns an MCP error "browser not connected" (D4 static-advertise: fails gracefully, no crash).
```

## 5. Execution flow (bootstrap → discovery → runtime)

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

Runtime  → see §4 for the detailed id-paired round-trip.
```

## 6. Findings & ownership

- The **agent→client REQUEST direction already existed downstream**: `openab-core`'s ACP connection
  receives `session/request_permission` from the agent and auto-replies it. So the server→client
  request work is *relaying* those upstream to the `/acp` client, not green-field.
- `session/new` / `session/resume` send `mcpServers: []`, but that path is **not** how the agent gets
  the tools (D2 — Cursor ignores ACP-passed MCP servers). Tools reach the agent via core's proxy HTTP
  MCP server registered in the agent's **native** config.
- **Ownership** — OpenAB side (`feat/acp-mcp-browser`): server→client request direction, generated
  typed wire, tunnel framing + core proxy. Extension side (katashiro): MCP server role over the
  outbound `/acp` WS + the DOM tools + executing in the active tab. Both: integration/e2e.

## 7. Tasks (as executed)

- **T0 spike** — confirm a CLI loads an HTTP MCP server from its native config, honours
  `tools/list_changed`, and handles a `tools/call` error gracefully.
- **T1** agent→client REQUEST direction (relay; gateway outbound request + pending-response map;
  read loop distinguishes client response vs request).
- **T2** migrate `acp_server` to generated typed wire (bidirectional surface).
- **T3** `session/request_permission` — **dropped** (D1 auto-approve).
- **T4** MCP-over-ACP tunnel framing (upstream only) — adopt the RFD (`mcp/connect` + `mcp/message` +
  `mcp/disconnect`); contract doc: [tunnel contract](../mcp-over-acp-tunnel-contract.md).
- **T5** OpenAB core = MCP proxy/aggregator (in-process HTTP MCP server; per-agent config injection;
  proxy as MCP client to the extension over the tunnel; tool-call routing; `rmcp` wiring).
- **T6** extension (katashiro) = MCP server + the five DOM tools, executing via `chrome.scripting`.
- **T7** integration + e2e (`scripts/acp-ws-smoke.py`) + deploy.

## 8. As-built (2026-07-20, OpenAB side wired end-to-end)

Realised call path (all in one `openab run` process):

```
agent tools/call ─http▶ core per-session ProxyHandler (mcp_proxy.rs)
   ─▶ BrowserTunnel (core trait) ─▶ RootBrowserTunnel (root, src/browser_tunnel.rs)
   ─▶ gateway AcpTunnelRegistry[channel_id] ─▶ TunnelHandle::mcp_message
   ═mcp/message═▶ extension    (only this hop leaves the pod)
```

Config injection is per-agent (`.cursor/mcp.json` / `.kiro/settings/mcp.json` merged at the session
workdir, loopback + bearer). The full loop (read_dom / screenshot / navigate / click / type + status
pill + reconnect on `session/resume`) was live-validated on a real deployment on 2026-07-20. A second
downstream delivery mode — `bridge` (stdio relay, `OPENAB_BROWSER_MODE`) — is also shipped; see the
[reverse-MCP ADR §6.3](./acp-server-websocket-reverse-mcp.md).

## 9. References

- **Mechanism:** [Reverse MCP-over-ACP over WebSocket](./acp-server-websocket-reverse-mcp.md)
  (roles, call route, architecture + sequence diagrams, multi-server generalization)
- [Base ADR](./acp-server-websocket-base.md) · [tunnel contract](../mcp-over-acp-tunnel-contract.md) ·
  [browser MCP agent setup](../browser-mcp-agent-setup.md)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)

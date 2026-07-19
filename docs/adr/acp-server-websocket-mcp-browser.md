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

### Findings that reshape the work
- The **agent→client REQUEST direction already exists on the downstream hop**:
  `openab-core/src/acp/connection.rs` receives `session/request_permission` from the agent
  and currently **auto-replies** it (~L252). So T1 is not green-field — it is *relaying*
  those downstream requests up to the `/acp` client (and the response back) instead of
  auto-answering them.
- `session/new` / `session/resume` currently send `mcpServers: []` (connection.rs L567/784).
  Giving the agent browser tools = **injecting a core-side proxy MCP server** into that list
  (T5) that tunnels `tools/*` to the extension.

### Ownership
- **OpenAB side** (`feat/acp-mcp-browser`): T1, T2, T4 (contract + core routing), T5.
- **Extension side** (katashiro): T6; plus the client halves of T3 (respond to
  `request_permission`) and T4 (serve MCP over the tunnel).
- **Both**: T7.

### Tasks

**T0 — Spike (do first; de-risks everything).** PoC: give `cursor-agent` a non-empty
`mcpServers` pointing at a mock MCP server and confirm the LLM actually discovers
(`tools/list`) and calls (`tools/call`) a tool; confirm a downstream agent→client request
can be relayed and answered. If this doesn't hold, the browser goal needs a different path.

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

**T3 — `session/request_permission` end-to-end.** Largely the first concrete case of T1
(relay the request, extension consent UX, relay the response) — folds into T1, not a
separate large task.

**T4 — MCP-over-ACP tunnel framing.**
- 4.1 Fix the **wire contract**: how MCP JSON-RPC (`tools/list` / `tools/call` / results)
  is multiplexed over `/acp` (method namespace, e.g. `_mcp/*`; request/response
  correlation; framing). 4.2 Gateway routes MCP-namespaced messages between the `/acp`
  client and core. 4.3 Contract doc (the spec the extension implements). 4.4 Mock-MCP-
  client-over-tunnel tests.

**T5 — OpenAB core = MCP proxy/aggregator.**
- 5.1 A core-side local MCP server (proxy) the agent connects to via `mcpServers`.
- 5.2 Inject that proxy into the downstream `mcpServers` (currently `[]`).
- 5.3 The proxy acts as an MCP *client* to the extension over the upstream tunnel.
- 5.4 Tool-call routing (agent → proxy → tunnel → extension → result → agent).
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
  7.3 Rebuild + redeploy Falcon. 7.4 Finalize this ADR.

### Suggested order
T0 spike → T1 (+ T3 as its first case) → T2 → **T4 (fix the wire contract)** → T5 → then
T6 in parallel against the contract → T7. The heavy items are T1 (the direction), T4/T5
(tunnel + proxy), and T6 (extension). Structured `tool_call` display (base ADR §6) is
parallel and non-blocking.

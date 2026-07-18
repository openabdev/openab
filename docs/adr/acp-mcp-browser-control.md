# ADR: Browser control via MCP-over-ACP (Phase 2, proposed)

- **Status:** Proposed (design only — not implemented)
- **Date:** 2026-07-18
- **Author:** @brettchien
- **Related:** [ACP Server over WebSocket — Base (as-built)](./acp-server-websocket-base.md),
  [ACP Server with WebSocket Transport](./acp-server-websocket.md) (Phase 2: Tool Calls &
  Permissions), [openab-agent MCP](./openab-agent-mcp.md)

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

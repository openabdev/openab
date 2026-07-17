# ADR: ACP Server over WebSocket — Phase 1 (as-built)

- **Status:** Accepted
- **Date:** 2026-07-17
- **Author:** @brettchien
- **Related:** [ADR: ACP Server with WebSocket Transport](./acp-server-websocket.md) (original proposal, @pahud)
- **Conformance:** official ACP Schema **v1.19.0** — see [acp-official-methods.md](../acp-official-methods.md)
- **Implementation:** this PR (revives and completes #1260)

---

## 1. Context

The original proposal ([acp-server-websocket.md](./acp-server-websocket.md)) defines
the full ACP-server vision across five phases. This ADR is the **as-built record of
Phase 1** — the concrete, **wire-conformant** primitive surface the implementation
ships and that future work should follow.

Scope: a standard-ACP **1:1 streaming chat** endpoint for real ACP clients (browser,
desktop, IDE, CLI) over WebSocket. Not in Phase 1: tool calls / permissions, client
fs/terminal methods, multi-agent fan-out, Streamable HTTP.

Design goal (per decision on 2026-07-17): **follow the official ACP guide** so
third-party ACP clients (Zed, JetBrains, …) interoperate — no custom method names.

## 2. Decision — Phase 1 primitive surface (ACP-conformant)

Transport: `GET /acp`, feature-gated `acp` + runtime `OPENAB_ACP_ENABLED`. Token auth
on the WS upgrade via timing-safe compare (`subtle::ConstantTimeEq`,
`OPENAB_ACP_AUTH_KEY`). JSON-RPC 2.0; non-`"2.0"` rejected with `-32600`.

### Client → Agent (requests)

| Method | Params | Result |
|---|---|---|
| `initialize` | `{ protocolVersion: 1, clientCapabilities?, clientInfo? }` | `{ protocolVersion: 1, agentCapabilities, agentInfo, authMethods: [] }` |
| `session/new` | `{ cwd, mcpServers }` | `{ sessionId }` |
| `session/resume` | `{ sessionId, cwd, mcpServers? }` | `{}` (no history replay) |
| `session/prompt` | `{ sessionId, prompt: [ContentBlock] }` | `{ stopReason }` |

`agentCapabilities` advertises `sessionCapabilities.resume` (we support resume) and
`loadSession: false` (we cannot replay history — see §3). `promptCapabilities` are
all `false` in Phase 1 (text only). `protocolVersion` is the integer `1`.

### Client → Agent (notification)

| Method | Params | Effect |
|---|---|---|
| `session/cancel` | `{ sessionId }` | one-way; in-flight prompt ends with `stopReason:"cancelled"`. No response. |

### Agent → Client (notification)

- `session/update` with `update.sessionUpdate = "agent_message_chunk"` and
  `update.content = { type:"text", text: <delta> }` — streamed reply text. Delta is
  sliced char-boundary-safe (`str::get`, never byte-index) so CJK / 顏文字 / emoji
  cannot panic the stream.
- Turn completion is the `session/prompt` **response** (`{ stopReason }`, correlated
  to the request id), not a separate notification. `stopReason` ∈ `end_turn` /
  `cancelled`. A backend timeout has no ACP stopReason, so it returns a JSON-RPC
  error (`-32603`) instead.

### Session ↔ core mapping

- `sessionId = sess_<uuid>` and `channel_id = acp_<uuid>` share one uuid, so
  `channel_id` is always re-derivable from a persisted `sessionId`.
- Prompts become a `GatewayEvent` (`platform:"acp"`, `channel:acp_<uuid>`); core
  keys continuity by `session_key = acp:<channel_id>`.

## 3. Resume — why `session/resume`, not `session/load`

ACP distinguishes `session/load` (agent **replays** history via `session/update`,
then responds) from `session/resume` (restores context, **MUST NOT** replay). We
implement **`session/resume`**, decided against `crates/openab-core/src/acp/pool.rs`:

- The conversation history lives inside the **downstream** coding-agent CLI's session
  (claude / codex / kiro). The core only persists a `thread_key → agent sessionId`
  mapping — it does **not** hold a replayable upstream transcript. So the gateway
  cannot satisfy `session/load`'s replay contract; `loadSession: false`.
- Continuation still works: on the next prompt, core recovers the underlying agent
  session via its persisted mapping + downstream `session/load` (this survives a
  process restart, within the agent's retention / `session_ttl_hours`, default 4h).
- `resume` therefore restores context without replay; the **client** keeps its own
  transcript for display. `session/resume` returns `{}` immediately.

Whether the core session is still alive is **not observable** at the gateway — an
expired session silently starts fresh, and the core prefixes its first reply with a
"Session expired" notice the client can surface.

Security: `sessionId` is a server-minted, high-entropy capability; `session/resume`
requires a well-formed `sess_<uuid>`, keeping the channel inside the `acp_` namespace
and rejecting forged ids.

## 4. Divergences from the original proposal

| Proposal (Phase 1) | As-built | Why |
|---|---|---|
| Add `agent-client-protocol` crate dep | removed — hand-rolled JSON-RPC | fewer deps; small surface |
| "Bearer token auth" | `subtle::ConstantTimeEq` + `OPENAB_ACP_AUTH_KEY` | timing-safe, no new dep |
| Resume in **Phase 3** | `session/resume` in **Phase 1** | core continuity is already channel-keyed + persisted, so a gateway-only change buys reconnect resume cheaply |

## 5. Consequences & limits

- **1:1 only** — reply registry is `channel_id → single reply_tx`; the delta stream
  assumes one monotonic text. Multi-agent fan-out is Phase 4 (would corrupt this).
- **cwd / mcpServers** — accepted on `session/new` / `session/resume` for wire
  conformance but not yet propagated into the agent (follow-up).
- **Emoji** — inline 顏文字 flow through as text; reaction emoji stay no-op in Phase 1.
- **Reconnect** — on WS disconnect the per-connection session map is dropped; the
  client reconnects with `session/resume` + its persisted `sessionId`.

## 6. Roadmap (from proposal; resume pulled into Phase 1)

- **Phase 2** — tool calls: `session/update` variants `tool_call` / `tool_call_update`,
  and `session/request_permission`; reaction emoji → updates
- **Phase 3** — `session/load` with history replay (needs an upstream transcript store)
- **Phase 4** — multi-agent fan-out
- **Phase 5** — Streamable HTTP (POST + SSE) on the same `/acp`

## 7. References

- Original proposal: [acp-server-websocket.md](./acp-server-websocket.md)
- Official method surface + coverage: [acp-official-methods.md](../acp-official-methods.md)

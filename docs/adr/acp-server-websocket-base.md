# ADR: ACP Server over WebSocket — Base (as-built)

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
the base** — the concrete, **wire-conformant** primitive surface the implementation
ships and that future work should follow.

Scope: a standard-ACP **1:1 streaming chat** endpoint for real ACP clients (browser,
desktop, IDE, CLI) over WebSocket. Not in the base: tool calls / permissions, client
fs/terminal methods, multi-agent fan-out, Streamable HTTP.

Design goal (per decision on 2026-07-17): **follow the official ACP guide** so
third-party ACP clients (Zed, JetBrains, …) interoperate — no custom method names.

## 2. Decision — the base primitive surface (ACP-conformant)

Transport: `GET /acp`, feature-gated `acp` + runtime `OPENAB_ACP_ENABLED`. Mounted on
**both** the standalone `openab-gateway` binary (`serve()`) **and** the embedded
gateway of `openab run` (the unified binary) — so fleet deployments that run
`openab run` (not the standalone gateway) serve ACP too. The embedded HTTP server
starts whenever `OPENAB_ACP_ENABLED` is set (or any platform / `[gateway]` is
configured) — so an ACP-only deployment, or one whose only platform is Discord (which
the core connects to directly, without the webhook server), still binds the listener.
ACP replies are routed back via the unified adapter's `dispatch_reply`
(`platform == "acp"`).

**Two independent auth layers:**

1. **Transport** — token on the WS upgrade, timing-safe compare (`OPENAB_ACP_AUTH_KEY`;
   unset ⇒ unauthenticated).
2. **Identity** — ACP events carry a fixed synthetic sender id `acp_client` and pass
   through the gateway trust registry (the `acp` platform is seeded there alongside
   telegram/line/…). Admit the sender with `GATEWAY_ALLOW_ALL_USERS=true` or
   `GATEWAY_ALLOWED_USERS=acp_client`; otherwise every prompt is denied with a
   "request-access" echo. (These must be **process** env on the broker, not
   `[agent].env`.)

JSON-RPC 2.0; non-`"2.0"` rejected with `-32600`.

### Client → Agent (requests)

| Method | Params | Result |
|---|---|---|
| `initialize` | `{ protocolVersion: 1, clientCapabilities?, clientInfo? }` | `{ protocolVersion: 1, agentCapabilities, agentInfo, authMethods: [] }` |
| `session/new` | `{ cwd, mcpServers }` | `{ sessionId }` |
| `session/resume` | `{ sessionId, cwd, mcpServers? }` | `{}` (no history replay) |
| `session/prompt` | `{ sessionId, prompt: [ContentBlock] }` | `{ stopReason }` |

`agentCapabilities` advertises `sessionCapabilities.resume` (we support resume) and
`loadSession: false` (we cannot replay history — see §3). `promptCapabilities` are
all `false` in the base (text only). `protocolVersion` is the integer `1`.

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

| Proposal (the base) | As-built | Why |
|---|---|---|
| Add `agent-client-protocol` crate dep | removed — hand-rolled JSON-RPC | fewer deps; small surface |
| "Bearer token auth" | `subtle::ConstantTimeEq` + `OPENAB_ACP_AUTH_KEY` | timing-safe, no new dep |
| Resume in **Phase 3** | `session/resume` in **the base** | core continuity is already channel-keyed + persisted, so a gateway-only change buys reconnect resume cheaply |

## 5. Consequences & limits

- **1:1 only** — reply registry is `channel_id → single reply_tx`; the delta stream
  assumes one monotonic text. This matches ACP's 1:1 nature (one client ↔ one agent) and
  is correct. Multi-agent "conversation" (Discord-style) is NOT fan-out and NOT an ACP
  concern: it is N independent OpenAB instances, each its own `/acp` connection, relayed
  by the client acting as the shared room (see §6, Not needed).
- **OpenAB command parity is mostly free** — control directives (`[[ws]]`, `[[model]]`, …)
  and slash commands (`/reset`, `/model`, …) are message-text conventions parsed
  platform-agnostically (`openab-core`), so they already work over ACP when the client
  includes them in a prompt — no ACP-specific work required. A typed UI for them
  (`authenticate` / `available_commands_update`) is an optional later nicety.
- **cwd / mcpServers** — accepted on `session/new` / `session/resume` for wire
  conformance but not yet propagated into the agent (follow-up).
- **Emoji** — inline 顏文字 flow through as text; reaction emoji stay no-op in the base.
- **Reconnect** — on WS disconnect the per-connection session map is dropped; the
  client reconnects with `session/resume` + its persisted `sessionId`.

## 6. Roadmap (re-scoped; not the original proposal's numbered phases)

North star: the agent's LLM autonomously operating the user's real browser (generalized
"computer use") — see [MCP-over-ACP browser control](./acp-server-websocket-mcp-browser.md).

### Critical path (next) — everything the browser goal requires
- **agent→client REQUEST direction** — the base does only client→agent + agent→client
  *notifications*; browser/tool use needs the agent to send *requests* to the client and
  await a result. The WS is already bidirectional; the dispatch loop must add this path.
- **`session/request_permission`** — tool-use approval.
- **MCP-over-ACP tunnel + OpenAB core as MCP proxy** — the extension exposes browser
  tools (MCP server role over its outbound WS); core proxies them to the in-pod agent.
- **Generated typed wire types (v1)** — decided for the base: adopt offline codegen
  (typify → plain serde, no `schemars` dep) rather than hand-rolling the expanded
  bidirectional surface. Currently hand-rolled; migration planned (validate round-trip
  against real traffic first).

### Optional (as-needed, off the critical path)
- richer `session/update` variants: `tool_call` / `tool_call_update` (display),
  `agent_thought_chunk` / `plan` / `available_commands_update` / `usage_update`
- `fs/*`, `terminal/*` (sibling agent→client capabilities)
- `ContentBlock` image / audio / resource (image only if screenshot-based browser tools)
- session admin: `session/close` / `list` / `delete`, `set_mode` / `set_config_option`,
  `session/load` (history replay — needs an upstream transcript store)
- typed command UI: `authenticate`, `available_commands_update` advertisement
- **Streamable HTTP** transport (POST + SSE on `/acp`) — only for environments where
  WebSocket is not viable (serverless, aggressive proxies); not needed for local/WS use
- multiple sessions per connection

### Not needed (removed from scope)
- **Multi-agent fan-out / ensemble** — Discord-style multi-agent is N independent OpenAB
  instances relayed by the client (a "room"): client-side orchestration, no ACP fan-out.
  ACP is 1:1; fan-out would only produce a single-agent "ensemble" answer, which is not a
  goal (you want to *see* the separate agents, not merge them).

### Observability (recommended first, low-risk)
- An **ACP trace mode** (flag-gated, both directions/hops) to record real ACP traffic —
  reveals the variant surface downstream agents actually emit, informs which of the
  Optional variants to forward, and validates the generated-type round-trip.

## 7. Typing & dependency decision (as-built: generated types vendored; trivial payloads hand-rolled + conformance-pinned)

Both sides of OpenAB's ACP started **hand-rolled, untyped** (`serde_json::Value` + manual
string matching on `sessionUpdate` variants): the upstream server here (~740 lines, chat
only) and the downstream client in `openab-core/src/acp/` (`protocol.rs` + `connection.rs`,
~1800 lines, many variants). Hand-rolling caused the exact conformance bugs fixed during the
base build (`agentMessageChunk`→`agent_message_chunk`, `stopReason` snake_case, integer
`protocolVersion: 1`).

**As-built (this PR).** The generated types now exist and are committed, but the switch was
made surgically per the rule below rather than as a blanket rewrite:

- **Generated types vendored + committed** — `crates/openab-gateway/src/adapters/acp_schema.rs`
  (feature-gated `acp`), produced by `cargo-typify 0.7.0` from the vendored ACP v1 schema
  (`crates/openab-gateway/schemas/acp-v1.schema.json`, pinned to upstream `schema.json`
  @ `eb88e992` / ACP Schema v1.19.0). Plain serde, **0** `schemars`/`serde_with` in the
  generated body (verified). The full v1 surface is generated (one closed dep graph — a
  hand-trimmed subset would not be meaningfully smaller and would diverge from the schema);
  the remainder beyond the chat subset is inert `dead_code` until the roadmap consumes it.
- **Trivial chat payloads stay hand-rolled** (`json!`) — they are correct and readable, and
  the typify construction ergonomics for them are poor (`AgentCapabilities` has no `Default`;
  `ContentBlock` is an untagged `VariantN`). Per the rule, we did **not** churn them into
  builder chains.
- **Conformance is pinned, not asserted by construction** — the `acp_conformance` test module
  in `acp_server.rs` deserializes every hand-rolled payload the server emits/accepts through
  the generated types and proves serde is a stable fixed point. Any casing/field/shape drift
  (the original bug class) now fails CI. This is the round-trip validation §6 called for.
- **Full typed *construction* migration is deferred** to the bidirectional / MCP-over-ACP
  surface (roadmap §6 Critical path), where hand-rolling actually breaks and the generated
  types earn their keep. The trivial base does not need it.

Options weighed for typing the wire:

| Option | New deps | Verdict |
|---|---|---|
| Hand-roll (current) | 0 | Fine for the trivial chat subset; error-prone for the big bidirectional surface |
| Full `agent-client-protocol` crate | ~105 (incl. a 2nd async runtime, async-io/smol) | **Never** — connection/role machinery unneeded (we have our own WS + GatewayEvent bridge) |
| `agent-client-protocol-schema` (types) | **+24** (measured 376→400), schemars-dominated | schemars is for `JsonSchema` derive we don't use at runtime; `serde_with`/`strum` mandatory (no feature to drop); floor is fixed |
| **Offline codegen (typify) → committed serde-only `.rs`** | **~0 runtime** | **Chosen & shipped** — `acp_schema.rs` generated + committed; typed conformance without the schemars tree |

Notes:
- **v1 only.** `v2` is experimental (`unstable_protocol_v2`, adds `diffy`), currently
  wire-identical to v1, "may change at any time". We negotiate `protocolVersion 1`.
- **Caveat (resolved for the chat subset):** ACP types lean on `serde_with` (MaybeUndefined
  tri-state, ~600 uses across the crate's v1 source), so a naive vendor-and-strip-`JsonSchema`
  is not clean and typify's plain-serde output must be **round-trip validated**. For the chat
  subset this is now **done** — the PoC and the `acp_conformance` test show the generated types
  round-trip the real wire exactly, with **no** `serde_with`/MaybeUndefined divergence (the one
  nuance: typify materializes schema-default capability booleans explicitly, which is
  semantically identical). Advanced bidirectional variants still warrant the same check via the
  §6 ACP trace mode before they are wired.
- **Rule:** hand-roll only the trivial; **generate the complex.** The switch point is the
  bidirectional/MCP surface. Highest ROI if unifying: the downstream core client
  (~1800 lines of manual variant matching), not this small upstream server.

## 8. References

- Original proposal: [acp-server-websocket.md](./acp-server-websocket.md)
- Official method surface + coverage: [acp-official-methods.md](../acp-official-methods.md)
- MCP-over-ACP browser control: [acp-server-websocket-mcp-browser.md](./acp-server-websocket-mcp-browser.md)

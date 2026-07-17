# ADR: ACP Server with WebSocket Transport

- **Status:** Proposed
- **Date:** 2026-06-30
- **Author:** @pahud
- **Related:** [ADR: Separate Binaries with Opt-In Unified Build](./unified-binary.md), [ADR: Multi-Platform Adapters](./multi-platform-adapters.md)
- **Implementation:** TBD

---

## 1. Context & Problem

OpenAB currently exposes agent capabilities through platform-specific adapters (Discord, Telegram, LINE, Teams, etc.) and a custom VTuber adapter that speaks OpenAI-compatible Chat Completions (PR #1234). Each adapter translates between the platform's protocol and OAB's internal `GatewayEvent`/`GatewayReply` schema.

This creates several limitations:

- **No standard protocol** — every new frontend (desktop app, VTuber skin, IDE plugin, CLI tool) requires a bespoke adapter
- **One-way translation** — the OpenAI-compatible endpoint is request/response only; the agent cannot initiate messages, request tool approval, or push structured state updates without a separate side-channel (Tier-2 WS)
- **No interoperability** — clients built for one agent platform cannot connect to OAB without custom integration work
- **Duplicated effort** — features like session management, tool call approval, and streaming are re-implemented per adapter

Meanwhile, the **Agent Client Protocol (ACP)** has emerged as an industry standard for editor↔agent communication, backed by Zed, JetBrains, GitHub, and Block (Goose). ACP defines:

- JSON-RPC 2.0 message format over stdio or WebSocket
- Session lifecycle (`initialize` → `session/new` → `session/prompt` → notifications)
- Bidirectional communication (agent can request permissions, push tool status)
- Streaming via notifications (`AgentMessageChunk`, `ToolCall`, `ToolCallUpdate`)
- Capability negotiation at connection time

ACP's WebSocket transport RFD is in draft with reference implementations already shipping (zeph-acp, acp-ws-bridge). The official Rust crate [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol) provides typed request/response/notification handling.

### Why OAB is uniquely positioned

Existing ACP servers (Anvil, Goose, codex-acp) are **local, single-agent** setups — they spawn one agent subprocess and drive it via stdio. No one is building a **remote, multi-agent ACP server** that:

- Routes requests to different coding agents (Codex, Claude, Kiro, etc.)
- Manages agent orchestration and sub-agent pipelines
- Provides platform-level memory, MCP bridging, and tool policies
- Serves multiple concurrent clients over the network

This is OAB's differentiation.

---

## 2. Decision

Implement an ACP-compliant server endpoint in the OpenAB unified binary, using WebSocket as the primary transport. The endpoint will be exposed at `GET /acp` with WebSocket upgrade, reusing the existing axum HTTP listener.

### Architecture

```
ACP Clients
  ├── IDE plugins (Zed, JetBrains)
  ├── Desktop apps (AniCompanion, VTuber skins)
  ├── Web apps (browser-based agent UI, web VTuber)
  ├── CLI tools
  └── Mobile apps
  │
  │── GET /acp (Upgrade: websocket) — WSS from any environment
  │── JSON-RPC over WebSocket (bidirectional)
  │
OpenAB Unified Binary
  ├── src/acp_server.rs          ← ACP JSON-RPC dispatch
  ├── agent-client-protocol crate ← official Rust SDK
  │
  ├── initialize → capability negotiation
  ├── session/new → create OAB session, select agent
  ├── session/prompt → dispatch to coding agent (or fan-out to multiple)
  ├── session/notification → stream AgentMessageChunk, ToolCall, etc.
  ├── requestPermission → tool approval flow
  │
  └── Internal: GatewayEvent / GatewayReply (existing OAB machinery)
        ├── agent container: Claude
        ├── agent container: Codex
        └── agent container: Kiro
```

### Client Scenarios

ACP is transport-agnostic at the application layer — any environment that supports WebSocket can be a client:

| Client Type | Example | Use Case |
|---|---|---|
| IDE plugin | Zed, JetBrains | Code editing with agent assist |
| Desktop app | AniCompanion | VTuber avatar driven by agent |
| Web app | Browser SPA (React/Vue) | Web-based agent UI, web VTuber with three-vrm |
| CLI | Custom shell tool | Headless automation, CI/CD |
| Mobile | iOS/Android app | On-the-go agent access |

Web apps are particularly interesting — browsers natively support WebSocket, so a web-based frontend can connect directly to OAB's ACP endpoint without any bridge or adapter. This enables building rich agent UIs (including browser-rendered 3D VRM characters via three.js) that talk to the full OAB multi-agent platform.

### Transport

- **WebSocket** (primary): `GET /acp` with `Upgrade: websocket` header. Full-duplex, persistent connection. All JSON-RPC messages as WebSocket text frames.
- **Streamable HTTP** (future, optional): Same `/acp` endpoint, POST for client→server, GET+SSE for server→client. Deferred to Phase 2.
- **stdio** (not applicable): OAB is a network service, not a subprocess.

### ACP Lifecycle Mapping

| ACP Method | OAB Internal Action |
|---|---|
| `initialize` | Negotiate capabilities, return `connectionId` |
| `session/new` | Create session, resolve target agent from config/model param |
| `session/load` | Resume existing session from OAB session store |
| `session/prompt` | Convert to `GatewayEvent`, dispatch to agent |
| `session/cancel` | Cancel in-flight agent turn |
| `session/notification` (outbound) | Derived from `GatewayReply` — text chunks, tool calls, state |
| `requestPermission` (outbound) | When agent needs tool approval, prompt client |

### Agent Routing

The `model` field in `session/new` or `session/prompt` maps to OAB's agent pool:

- `openab` (default) → OAB's configured default agent
- `codex::*` → Codex agent container
- `claude::*` → Claude agent container
- `kiro::*` → Kiro agent container

This reuses OAB's existing multi-agent dispatch and session routing.

### Multi-Agent Fan-Out (Future)

One unique capability of OAB as an ACP server is fan-out to multiple agents with result aggregation. A single `session/prompt` can be dispatched to multiple agents in parallel:

```
ACP Client → session/prompt "review this code"
                │
OAB dispatcher ─┼── Claude → findings A
                ├── Codex  → findings B
                └── Kiro   → findings C
                │
         aggregate → merged result → AgentMessageChunk stream back to client
```

This is analogous to how OAB's 法師 team currently reviews PRs (multiple agents review in parallel, results are synthesized). With ACP, this multi-agent orchestration becomes accessible to any client — not just Discord.

Modes:
- **Single agent** (default): one session, one agent — same as today
- **Multi-session**: client opens N sessions to N agents concurrently (ACP spec supports multiple sessions per connection)
- **Fan-out aggregate**: OAB internally dispatches to multiple agents, returns a unified response (requires OAB-level orchestration logic)

### Feature Flag

```toml
[features]
unified = ["telegram", "line", "feishu", "googlechat", "wecom", "teams", "vtuber", "acp"]
acp = ["dep:agent-client-protocol", "dep:openab-gateway", "dep:axum"]
```

Enable with `OPENAB_ACP_ENABLED=true`. Auth via `OPENAB_ACP_AUTH_KEY` (Bearer token on WebSocket upgrade).

---

## 3. Consequences

### Positive

- **Any ACP client connects to OAB** — Zed, JetBrains, desktop apps (AniCompanion), custom CLIs, bots
- **Standard protocol** — no more bespoke adapters for each new frontend
- **Bidirectional by design** — agent can request permission, push notifications, initiate conversations
- **Rich tool visibility** — clients get structured `ToolCall`/`ToolCallUpdate` events, not just flattened text
- **Session persistence** — clients can disconnect and resume via `session/load`
- **Ecosystem leverage** — official SDKs in Rust, TypeScript, Python, Kotlin, Java; no custom client libraries needed
- **VTuber superseded** — ACP subsumes both Tier-1 (streaming) and Tier-2 (state events) of the VTuber adapter; existing VTuber skins can migrate to ACP or continue using the OpenAI-compatible endpoint

### Negative

- **Dependency on external spec** — ACP is still evolving (WebSocket transport is "Draft" status); we may need to track spec changes
- **Complexity** — adds another endpoint to the unified binary alongside existing adapters
- **Not all clients speak ACP yet** — VTuber skins (AniCompanion, Open-LLM-VTuber) currently only speak OpenAI Chat Completions; adoption requires upstream PRs or an ACP→OpenAI bridge mode

### Neutral

- Existing platform adapters (Discord, Telegram, etc.) are unaffected — they continue using the internal gateway protocol
- The OpenAI-compatible VTuber endpoint can coexist as a simpler alternative for clients that don't need full ACP capabilities

---

## 4. Alternatives Considered

### A. Custom WebSocket protocol (rejected)

Design our own JSON-over-WebSocket protocol for VTuber/desktop clients.

**Why rejected:** Reinvents what ACP already standardizes. No ecosystem leverage, no existing SDKs, no interop with editors. More maintenance burden.

### B. OpenAI Realtime API (rejected)

Adopt OpenAI's Realtime API WebSocket protocol.

**Why rejected:** Designed for voice/multimodal streaming, not agent↔client tool use. Missing session management, tool approval flows, and capability negotiation. Proprietary.

### C. MCP Streamable HTTP only (rejected)

Use MCP's transport layer for client communication.

**Why rejected:** MCP is for agent↔tool communication (agent calls tools on MCP servers), not client↔agent. Different abstraction level. ACP already builds on top of MCP where appropriate.

### D. Keep adapter-per-frontend approach (rejected)

Continue building bespoke adapters (VTuber, future desktop app, future CLI).

**Why rejected:** Doesn't scale. Each adapter re-implements session management, streaming, tool display. ACP gives us one implementation that works for all frontends.

---

## 5. Implementation Plan

### Phase 1: Core ACP Server (MVP)

- Add `agent-client-protocol` crate dependency
- Implement WebSocket upgrade at `GET /acp`
- `initialize` / `session/new` / `session/prompt` / `session/cancel`
- Map `GatewayReply` to `AgentMessageChunk` notifications
- Bearer token auth on upgrade
- Feature-gated behind `acp`

### Phase 2: Tool Calls & Permissions

- Forward agent tool calls as `ToolCall` notifications
- Implement `requestPermission` for dangerous operations
- `ToolCallUpdate` with status and file locations

### Phase 3: Multi-Session & Resume

- Support multiple sessions per connection
- `session/load` with conversation history replay
- Session persistence in OAB's existing session store

### Phase 4: Multi-Agent Fan-Out

- Fan-out a single prompt to multiple agents
- Aggregate results into a unified response
- Expose as a special `model: "openab::ensemble"` or similar

### Phase 5: Streamable HTTP (optional)

- Add HTTP transport (POST + SSE) on the same `/acp` endpoint
- For environments where WebSocket is not viable (serverless, aggressive proxies)

---

## 6. References

- [Agent Client Protocol Spec](https://agentclientprotocol.com)
- [ACP WebSocket Transport RFD](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport)
- [Official Rust SDK](https://crates.io/crates/agent-client-protocol)
- [ACP GitHub Repo](https://github.com/agentclientprotocol/agent-client-protocol) (3.5k stars)
- [Goose ACP Implementation](https://block-goose.mintlify.app/advanced/acp-protocol)
- [Brokk Anvil](https://github.com/brokkai/anvil) — Rust ACP server (45K SLoC)
- [acp-ws-bridge](https://crates.io/crates/acp-ws-bridge) — WebSocket↔stdio bridge
- [PR #1234: VTuber Adapter](https://github.com/openabdev/openab/pull/1234) — current OpenAI-compatible approach

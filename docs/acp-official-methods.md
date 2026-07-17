# ACP — Official Method Surface & OpenAB Coverage

Reference list of the **official** Agent Client Protocol methods/notifications, and
how OpenAB's Phase 1 ACP server (`docs/adr/acp-server-websocket-phase1.md`) maps onto
them. Phase 1 targets **wire conformance** for the chat subset, so standard ACP
clients (Zed, JetBrains, …) interoperate.

### Provenance / version pin

This table is built against a specific ACP revision — pin it so future diffs are
traceable:

| Field | Value |
|---|---|
| Spec docs | <https://agentclientprotocol.com/protocol/overview>, <https://agentclientprotocol.com/protocol/schema> |
| Governance repo | <https://github.com/agentclientprotocol/agent-client-protocol> |
| **Schema release** | **v1.19.0** (latest on GitHub releases as of fetch date) |
| **Rust crate release** | **v1.4.0** |
| Fetched | 2026-07-17 |
| Wire `protocolVersion` | integer **`1`** (single MAJOR version, negotiated at `initialize`) |

> When re-checking conformance later, bump this block to the new Schema/crate release
> and re-diff the tables below.

Directions use the ACP roles: the **Agent** answers prompts (here, OpenAB); the
**Client** is the app/UI (browser, Zed, CLI).

## Agent methods (Client → Agent, request/response)

| Method | Purpose | OpenAB Phase 1 |
|---|---|---|
| `initialize` | Negotiate protocol + capabilities | ✅ conformant (`protocolVersion:1`, `agentCapabilities`, `authMethods:[]`) |
| `authenticate` | Authenticate via a declared auth method | ⛔ (we use a pre-connect token on the WS upgrade; `authMethods:[]`) |
| `logout` | Drop authenticated state | ⛔ |
| `session/new` | Create a new session | ✅ (`{cwd, mcpServers}` accepted; returns `{sessionId}`) |
| `session/load` | Load a session **with** history replay | ⛔ **by design** — `loadSession:false` (no upstream transcript to replay; see ADR §3) |
| `session/resume` | Resume a session **without** replay | ✅ (`{sessionId, cwd, mcpServers?}` → `{}`) |
| `session/prompt` | Process a user prompt | ✅ (streams `session/update`, returns `{stopReason}`) |
| `session/close` | Close a session | ⛔ (cleanup on WS disconnect) |
| `session/list` | List known sessions | ⛔ |
| `session/delete` | Delete a session | ⛔ |
| `session/set_config_option` | Set a session config option | ⛔ |
| `session/set_mode` | Set the session mode | ⛔ |

## Notifications

| Method | Direction | Purpose | OpenAB Phase 1 |
|---|---|---|---|
| `session/cancel` | Client → Agent | Cancel in-flight work (one-way, no response) | ✅ conformant (notification; prompt ends `stopReason:"cancelled"`) |
| `session/update` | Agent → Client | Stream session events | ✅ `agent_message_chunk` (text). Other variants (`tool_call`, `tool_call_update`, `plan`, …) are Phase 2 |
| `$/cancel_request` | Bidirectional | Cancel an in-flight JSON-RPC request | ⛔ |

## Client methods (Agent → Client, request/response)

The agent runs server-side with its own fs/terminal, so OpenAB does not call any of
these in Phase 1.

| Method | Purpose | OpenAB Phase 1 |
|---|---|---|
| `session/request_permission` | Ask the client to approve a tool call | ⛔ (Phase 2) |
| `fs/read_text_file` / `fs/write_text_file` | Read/write a text file on the client | ⛔ |
| `terminal/create` / `output` / `wait_for_exit` / `kill` / `release` | Drive a client terminal | ⛔ |

## Conformance status (Phase 1)

The chat subset is **wire-conformant** with ACP Schema v1.19.0:

- `initialize` → integer `protocolVersion:1`, official `agentCapabilities` shape
  (`sessionCapabilities.resume`, `loadSession:false`, `promptCapabilities`), `authMethods:[]`.
- Streaming → `session/update` + `sessionUpdate:"agent_message_chunk"` + `content` ContentBlock.
- `stopReason` → official snake_case (`end_turn` / `cancelled`).
- `session/cancel` → one-way notification.
- Resume → `session/resume` (no replay), gated by `sessionCapabilities.resume`.

### Intentional non-support (documented, not a gap to close in Phase 1)

- **`session/load`** — needs an upstream conversation transcript OpenAB does not keep
  (history lives in the downstream agent CLI). Advertised as `loadSession:false`.
- **`authenticate`/`logout`, ContentBlock non-text, tool-call updates,
  `request_permission`, fs/terminal, session admin (`list`/`delete`/config/mode)** —
  deferred to later phases per the roadmap.

### To verify against a live client

- Field-level exactness of `agentCapabilities` / `clientCapabilities` sub-objects and
  the `ContentBlock` variants beyond `text`, by connecting a real ACP client (e.g.
  Zed) and diffing the `initialize` + `session/prompt` exchange against the schema.

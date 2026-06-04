# MCP Spec Alignment Checklist

Conformance audit of `openab-agent`'s MCP client against the
[MCP 2025-11-25 specification](https://modelcontextprotocol.io/specification/2025-11-25).
Each section below links to the authoritative `.mdx` source file in
[`modelcontextprotocol/modelcontextprotocol@main/docs/specification/2025-11-25/`](https://github.com/modelcontextprotocol/modelcontextprotocol/tree/main/docs/specification/2025-11-25);
every item in a section is derived from that file.

| | |
|---|---|
| Spec version audited | 2025-11-25 |
| Spec source | [`docs/specification/2025-11-25/`](https://github.com/modelcontextprotocol/modelcontextprotocol/tree/main/docs/specification/2025-11-25) |
| Codebase | _TBD_ |
| SDK | rmcp 1.7.0 |
| Last refreshed | 2026-06-04 |

## Automated conformance

An official wire-level conformance test framework exists at
[`modelcontextprotocol/conformance`](https://github.com/modelcontextprotocol/conformance)
(npm `@modelcontextprotocol/conformance`, TypeScript, actively maintained — v0.2.0-alpha as of 2026-06-03).
It supports MCP 2025-11-25 as a `--spec-version` target and ships SEP→check
traceability (`src/seps/*.yaml` + `traceability.json`) that maps directly back to spec
requirements, so results can be joined onto rows in this checklist.

**Mode**: `npx @modelcontextprotocol/conformance client --command "<bin> [args]" --spec-version 2025-11-25 --suite core`.
The framework spawns the client as a subprocess, hosts a mock test server, and
drives scenarios via the `MCP_CONFORMANCE_SCENARIO` env var — language-agnostic, so a
Rust client is fine.

**Viability for `openab-agent`**: feasible but requires:
1. Streamable HTTP client transport (`rmcp`'s HTTP transport) — `stdio`-only clients can't run it.
2. A thin Rust dispatch binary (the conformance equivalent of TS `everything-client.ts`)
   that reads `MCP_CONFORMANCE_SCENARIO` and routes to the right behaviour.
3. OAuth scenarios (DCR / PKCE / scope / issuer / token-endpoint-auth) are a
   large fraction of the client suite; gaps there will surface as expected failures
   until `rmcp` covers them.

## Legend

| Symbol | Meaning |
|---|---|
| ✅ | Implemented (directly in openab-agent or correctly delegated to SDK) |
| ⚠️ | Partial — present but with gaps vs spec, or spec keyword is RECOMMENDED/MAY and only minimally satisfied |
| ❌ | Not implemented |
| N/A | Server-side requirement (we are a client) or otherwise not applicable to this implementation |

## rmcp 1.7.0 Leverage Map

Cross-cuts the per-section Improvement Plans. Classifies the actionable **code** items
(doc/tracking-only items omitted) by how much rmcp 1.7.0 already provides. Verified
against the SDK source (`rmcp-1.7.0/src/`) and confirmed by Mira retroactive review
(2026-06-04). Symbols here are orthogonal to the Legend above.

| Symbol | Meaning |
|---|---|
| 🟢 | rmcp-provided — SDK does the heavy lifting; openab-agent effort = adopt / switch callsite / config wiring |
| 🟡 | wiring-only — peer methods / handler hooks exist in rmcp; effort = ACP bridge + the named `ClientHandler` struct |
| 🔴 | openab-native — rmcp does not help; full implementation in openab-agent |

### 🟢 rmcp-provided

- **Request timeout + auto-cancellation** (Rows 69-71, 70, 109, 518; Section 4 "consolidate" item) — `PeerRequestOptions { timeout }` + `await_response` auto-emit `CancelledNotification` (`reason="request timeout"`) then return `ServiceError::Timeout` (`service.rs:321-347`). Client side has **no** `call_tool_with_timeout` convenience (server-only); route via `Peer::send_request_with_option`. ~50 LOC callsite switch at `meta_tool.rs:98,122`.
- **OAuth discovery + step-up + audience binding** (Rows 153-168 PRM/AS-metadata, 161-162/207 `WWW-Authenticate` step-up, 183-190/211/242 RFC 8707 `resource`, 220-223 PKCE-method check) — all collapse into **adopt `AuthorizationManager`** (`transport/auth.rs:602`): ships PRM-first discovery (SEP-985) + RFC 9728/8414/OIDC, `ScopeUpgradeConfig` (default `auto_upgrade:true`, `max_upgrade_attempts:3`) for 401/403 step-up, and RFC 8707 `resource` via `.add_extra_param("resource", …)`. Shrinks the originally-estimated ~300-500 LOC PRM gap to "adopt + wire SSRF allowlist + assert `code_challenge_methods_supported` ⊇ S256".
- **stdio shutdown ladder** (Row 66) — `TokioChildProcess::graceful_shutdown()` (`child_process.rs:110-136`): close stdin → `select!` `child.wait()` vs 3s sleep → `child.kill()`.
- **stderr capture** (Row 79) — `TokioChildProcess::stderr(Stdio::piped())`.
- **Completion `_meta` carry-through** (Section 14 §3) — `CompleteRequestParams.meta` already preserved.

### 🟡 wiring-only (gated behind the named `ClientHandler`)

Replacing the blanket `()` handler at `runtime.rs:1066,1079` with a named struct unblocks
all of these; every hook already exists in rmcp `handler/client.rs`:

- `create_message` → route to `LlmProvider` (sampling, Row 390 / line 673)
- `on_tool_list_changed` (Row 503), `on_prompt_list_changed` (prompts §3), `on_resource_updated` (resources §3), `on_url_elicitation_notification_complete` (elicitation §5)
- `list_roots → -32601` and `create_elicitation → -32602` explicit declines (Rows 614, 763)
- prompts / resources / completion / logging surfaces (§2 of each) — `peer.get_prompt`, `list_all_prompts`, `read_resource`, `subscribe`, `complete_*`, `set_level` all exist (`service/client.rs:356-535`); effort = call + ACP bridge
- `progressToken` emission + `on_progress` routing (Row 255); log cancellation reason (Row 252)
- verify-only: `MCP-Protocol-Version` header on subsequent / GET-stream requests + SSE `retry` honoured (Rows 51, 101, 139)

### 🔴 openab-native (rmcp does not help)

- The named `ClientHandler` struct itself — rmcp provides the trait; we write the impl (keystone for the 🟡 group)
- `is_error` propagation to outer meta-tool result (Row 506, `agent.rs:184`)
- capability gating before `call_tool` / `list_all_tools` (Row 65)
- JSON Schema 2020-12 dialect validation (Rows 18-21 — rmcp confirmed to ship no validator)
- concise `Error.message` helper (Row 37b); tool projection / `taskSupport` / `outputSchema` validation (Rows 484, 492, 501)
- HTTPS/localhost provider enforcement (Rows 217-218); keyring token storage (Row 213)
- audit logging (Row 519); secret scrubbing (§17.4); HITL / consent hooks (Rows 509/515/516, §17.6); receive-side rate limits (Rows 268, 409, Section 15 §3)

---

## Base Protocol & JSON-RPC

Source: [`basic/index.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/index.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 1 | All implementations MUST support base protocol and lifecycle management | MUST | ✅ | `rmcp` 1.7 SDK — `src/mcp/runtime.rs:20-23` (`rmcp::ServiceExt`, `serve()`) |
| 2 | Other components MAY be implemented per app needs | MAY | ✅ | meta-tool gateway exposes `tools/list` + `tools/call` (`src/mcp/meta_tool.rs`); other components intentionally deferred per ADR §5 |
| 3 | All messages MUST follow JSON-RPC 2.0 | MUST | ✅ | `rmcp` SDK (delegated) |
| 4 | Requests MUST include a string or integer ID | MUST | ✅ | `rmcp` SDK (`RequestId`) |
| 5 | Request ID MUST NOT be `null` | MUST NOT | ✅ | `rmcp` SDK — Mira confirmed `RequestId` enum = `String(String) \| Integer(i64)` (no `Null` variant), so `"id": null` fails deserialization |
| 6 | Request ID MUST NOT be previously used by the requestor within the same session | MUST NOT | ✅ | `rmcp` SDK — Mira confirmed atomic counter for client-side request id generation |
| 7 | Result responses MUST include the same ID as the request | MUST | ✅ | `rmcp` SDK |
| 8 | Result responses MUST include a `result` field | MUST | ✅ | `rmcp` SDK |
| 9 | The `result` MAY follow any JSON object structure | MAY | ✅ | `rmcp` SDK passes through |
| 10 | Error responses MUST include the same ID as the request (except when malformed) | MUST | ✅ | `rmcp` SDK |
| 11 | Error responses MUST include an `error` field with `code` and `message` | MUST | ✅ | `rmcp` SDK (`ErrorData`) |
| 12 | Error codes MUST be integers | MUST | ✅ | `rmcp` SDK |
| 13 | Notification receivers MUST NOT send a response | MUST NOT | ✅ | `rmcp` SDK |
| 14 | Notifications MUST NOT include an ID | MUST NOT | ✅ | `rmcp` SDK |
| 15 | HTTP-based transports SHOULD conform to Authorization spec | SHOULD | ✅ | `StreamableHttpClientTransport` `src/mcp/runtime.rs:21-22`; OAuth in `src/mcp/oauth.rs` + paste flow `src/mcp/flow.rs` |
| 16 | STDIO transports SHOULD NOT follow auth spec; retrieve credentials from environment | SHOULD NOT | ✅ | `src/mcp/runtime.rs:1060-1064` (`env_clear()` + explicit `envs(stdio_child_env(&env))` + `TokioChildProcess::new`); resolver `src/mcp/config.rs:163-167` |
| 17 | Clients/servers MAY negotiate custom auth | MAY | N/A | not implemented; we use only spec-defined transports |
| 18 | Implementations MUST support JSON Schema 2020-12 for schemas without explicit `$schema` | MUST | ⚠️ | no in-code dialect handling; Mira confirmed `rmcp` 1.7.0 does NOT pull `jsonschema` / `valico` (only `schemars` for generation + `serde` for deserialization) — no dialect validation either layer |
| 19 | Implementations MUST validate schemas according to declared/default dialect | MUST | ❌ | no schema validation in `openab-agent` — tool input schemas passed through to LLM as-is (`describe_tool` relays `tool_def.input_schema` unvalidated at `src/mcp/meta_tool.rs:160`; `fetch_tools` at `src/mcp/meta_tool.rs:116-131`); confirmed by Mira via `Cargo.lock` audit |
| 20 | Implementations MUST handle unsupported dialects gracefully (return error indicating unsupported) | MUST | ❌ | no dialect detection / error path |
| 21 | Implementations SHOULD document which schema dialects they support | SHOULD | ❌ | not documented in README / docs |
| 22 | Schemas MUST be valid according to their declared or default dialect | MUST | N/A | server-authored; we are a client |
| 23 | Implementors are RECOMMENDED to use JSON Schema 2020-12 | RECOMMENDED | N/A | we don't author schemas |
| 24 | Implementations MUST NOT make assumptions about values at reserved `_meta` keys | MUST NOT | ✅ | no `_meta` references in `src/mcp/**`; we never read or rewrite the field, so by omission we don't assume |
| 25 | `_meta` prefix format MUST be dot-separated labels followed by `/` | MUST | N/A | we don't author `_meta` |
| 26 | `_meta` prefixes containing `modelcontextprotocol` / `mcp` reserved | (reserved) | N/A | we don't author |
| 27 | `_meta` name MUST begin and end with alphanumeric | MUST | N/A | we don't author |
| 28 | `_meta` name MAY contain `-`, `_`, `.`, alphanumerics | MAY | N/A | we don't author |
| 29 | SHOULD use reverse DNS notation for `_meta` prefixes | SHOULD | N/A | we don't author |
| 30 | Icon-rendering clients MUST support `image/png` and `image/jpeg` | MUST | N/A | `openab-agent` is a CLI/meta-tool gateway; no icon rendering surface |
| 31 | Icon-rendering clients SHOULD support `image/svg+xml` and `image/webp` | SHOULD | N/A | same |
| 32 | Icon consumers MUST take security precautions | MUST | N/A | same |
| 33 | Clients MUST reject icon URIs with unsafe schemes | MUST | N/A | same |
| 34 | MAY set image size limits | MAY | N/A | same |
| 35 | Icons SHOULD be fetched without credentials | (security) | N/A | same |
| 36 | MAY disallow file types | MAY | N/A | same |
| 37 | Validate MIME via magic bytes / allowlist | (security) | N/A | same |
| 37a | Verify icon URIs same-origin | (security) | N/A | same |
| 37b | JSON-RPC `Error.message` SHOULD be concise single sentence | SHOULD | ⚠️ | wrapped via `anyhow::Context` + `format!`; multi-clause sites confirmed: `src/mcp/runtime.rs:875, 972` (HTTP error body format), `src/mcp/config.rs:155-157` (spec config `read_to_string` + `serde_json::from_str` with two `with_context` layers), `src/mcp/config.rs:170-173` (env var resolve via `interpolate_value` + `with_context`), `src/mcp/meta_tool.rs:95+` (tool-call params), `src/mcp/oauth.rs:57,63` (PKCE/OAuth `anyhow!` sites) — Mira-extended inventory; Jelly fact-check 2026-06-03 dropped stale `runtime.rs:272` (not an error site, struct-literal line) + repointed `config.rs:149-150` → `155-157`, `config.rs:166` → `170-173` |

### Improvement Plan (Jelly + Mira consensus, section 0)

- [ ] **Rows 18-21 (JSON Schema 2020-12)**: rmcp 1.7.0 confirmed not to validate dialects (no `jsonschema` / `valico` in `Cargo.lock`; only `schemars` for gen + `serde` for deserialize). Decide: (a) add a thin validator at `src/mcp/meta_tool.rs::fetch_tools` boundary using `jsonschema` crate, OR (b) document this as a known limitation in README and surface unsupported-dialect tool entries as `NeedsAttention`. Either way, document supported dialect in `openab-agent/docs/`.
  - **Eval**: openab-agent layer (rmcp doesn't own schema-dialect validation and probably shouldn't — it's a serialization SDK) · option (a) drop-in with `jsonschema` crate (~100 LOC) but compliance theatre because we pass schemas straight to the LLM which tolerates any dialect; option (b) docs only · **fit: borderline — recommend (b)**. Adding a validator for tool-schemas we just relay isn't a meaningful gate.
- [ ] **Rows 19-20**: surface unsupported-dialect tool entries as `NeedsAttention` rather than silently passing through; covers MUST-handle-gracefully.
  - **Eval**: openab-agent only (extends our existing `ServerStatus` model, mirrors `NeedsAuth`) · drop-in (~30 LOC) · **fit: in-scope**. Even if we skip row 18 validation, surfacing "we don't know this dialect" is the bare-minimum graceful handling the MUST asks for.
- [ ] **Row 37b (`Error.message` brevity)**: introduce a `concise_error_message(err: &anyhow::Error) -> String` helper that takes the top-level cause's `to_string()` (not the chained one) for the JSON-RPC error payload, and keep the full chain only in `tracing` logs. Audit sites: `runtime.rs:875, 972`, `config.rs:155-157`, `config.rs:170-173`, `meta_tool.rs:95+`, `oauth.rs:57,63`.
  - **Eval**: openab-agent only · drop-in (one helper + call-site touch-ups, ~80 LOC) · **fit: defer — low value**. SHOULD, not MUST. anyhow chains are arguably useful debug context; only do this if a real server / LLM complains about verbose `error.message`s.
- [ ] **Documentation**: README MCP section should explicitly call out (a) JSON Schema dialect support / passthrough behavior, (b) that `_meta` keys are passed through opaquely, (c) icon-rendering is N/A (we're not a UI client).
  - **Eval**: docs only · drop-in · **fit: in-scope**. Cheap; sets reader expectation.

## Lifecycle

Source: [`basic/lifecycle.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/lifecycle.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 38 | Initialization phase MUST be first interaction | MUST | ✅ | `rmcp::ServiceExt::serve()` enforces handshake at `src/mcp/runtime.rs:1066,1079` |
| 39 | Client MUST initiate the `initialize` request | MUST | ✅ | `rmcp` SDK (client role via `RoleClient` at `src/mcp/runtime.rs:67`) |
| 40 | `initialize` carries `protocolVersion`, `capabilities`, `clientInfo` | (field) | ✅ | `rmcp` SDK; our client handler is `()` (unit) at `runtime.rs:1066,1079` so `capabilities` payload is empty |
| 41 | Server MUST respond with `protocolVersion`, `capabilities`, `serverInfo` | MUST | N/A | server-side requirement |
| 42 | After successful initialization, client MUST send `notifications/initialized` | MUST | ✅ | `rmcp` SDK |
| 43 | Client SHOULD NOT send other requests pre-init except ping | SHOULD NOT | ✅ | `rmcp` SDK (we only call `peer.list_all_tools()` / `peer.call_tool()` after `serve().await` returns Ok) |
| 44 | Server SHOULD NOT send other requests pre-init except ping/logging | SHOULD NOT | N/A | server-side |
| 45 | Client MUST send a supported `protocolVersion` | MUST | ✅ | `rmcp` SDK (sends `rmcp::model::ProtocolVersion::default()` which is the latest version the SDK knows) |
| 45a | Client MAY support older `protocolVersion` values | MAY | ⚠️ | controlled by `rmcp` 1.7 internal version policy; not configurable from our code |
| 46 | Client SHOULD send the latest version it supports | SHOULD | ✅ | `rmcp` SDK (default protocol version constant) |
| 47 | If server supports requested version, it MUST echo | MUST | N/A | server-side |
| 48 | Otherwise server MUST respond with another supported version | MUST | N/A | server-side |
| 49 | Server SHOULD respond with its latest supported version | SHOULD | N/A | server-side |
| 50 | If client does not support server's response version, client SHOULD disconnect | SHOULD | ⚠️ | `rmcp` `serve()` returns `Err` on incompatible version; we surface via `with_context(\|\| format!("mcp handshake with ..."))` at `runtime.rs:1068,1081` and mark `ServerStatus::Failed(...)` — disconnect implicit via dropping handle |
| 50a | spec-conflict — schema says MUST | (spec-conflict) | N/A | not actionable; upstream should reconcile |
| 51 | HTTP: client MUST include `MCP-Protocol-Version` header on subsequent requests | MUST | ✅ | `rmcp::transport::StreamableHttpClientTransport` SDK responsibility (`runtime.rs:21-22,1073-1077`); Section 2 row 139 verified rmcp worker extracts `init_result.protocol_version` and injects into `protocol_headers` used for every subsequent POST (SDK `transport/streamable_http_client.rs:408-418, 511-522`). Jelly fact-check 2026-06-03 upgraded ⚠️→✅ |
| 52 | Client capability: `roots` (listChanged optional) | (capability) | ❌ | bare `()` handler at `runtime.rs:1066,1079` — no `roots` advertised; servers depending on roots cannot constrain themselves |
| 53 | Client capability: `sampling` | (capability) | ❌ | no client `sampling` handler — servers cannot request LLM sampling from us |
| 54 | Client capability: `elicitation` | (capability) | ❌ | no elicitation handler — servers cannot ask user for form/URL input via us |
| 55 | Client capability: `tasks` | (capability) | ❌ | no task-augmentation declaration |
| 56 | Client capability: `experimental` | (capability) | ❌ | none declared |
| 57 | Server capability: `prompts` | (capability) | N/A | server-side declaration |
| 58 | Server capability: `resources` | (capability) | N/A | server-side |
| 59 | Server capability: `tools` | (capability) | N/A | server-side; we consume via `peer.list_all_tools()` at `src/mcp/meta_tool.rs:122` + `peer.call_tool()` at `src/mcp/meta_tool.rs:98` |
| 60 | Server capability: `logging` | (capability) | N/A | server-side |
| 61 | Server capability: `completions` | (capability) | N/A | server-side |
| 62 | Server capability: `tasks` | (capability) | N/A | server-side |
| 63 | Server capability: `experimental` | (capability) | N/A | server-side |
| 64 | Both parties MUST respect the negotiated protocol version | MUST | ✅ | `rmcp` SDK |
| 65 | Both parties MUST only use successfully negotiated capabilities | MUST | ⚠️ | we only call `tools/list` + `tools/call`; we do NOT gate on server-advertised capabilities — assume `tools` cap is always present, no check before `meta_tool.rs:122` invocation |
| 66 | stdio shutdown: client SHOULD close stdin, wait, SIGTERM, then SIGKILL | SHOULD | ⚠️ | `McpRuntimeManager::disconnect()` (`runtime.rs:267`) now drives explicit teardown: drops the peer `Arc` + flips `Disconnected`, then `cancellation_token().cancel()` → rmcp serve-loop `QuitReason::Cancelled` → `transport.close()` → `TokioChildProcess::graceful_shutdown` (close stdin → 3s grace → SIGKILL). **Partial**: rmcp's `graceful_shutdown` omits the SIGTERM rung and a tunable grace, and the shared `Arc` makes `close()`/`cancel()` (which would `await` the child reap) unreachable — full ladder needs rmcp upstream |
| 67 | Server MAY initiate stdio shutdown | MAY | N/A | server-side |
| 68 | HTTP shutdown by closing associated HTTP connection(s) | (transport) | ✅ | `rmcp::transport::StreamableHttpClientTransport` connection lifecycle (drop) |
| 69 | Implementations SHOULD establish timeouts on all sent requests | SHOULD | ✅ | both request sites route through `PeerRequestOptions { timeout: Some(d) }` → `send_request_with_option(..).await_response()`: `tools/call` at `meta_tool.rs` `call_tool`, `tools/list` at `meta_tool.rs` `fetch_tools`; `d` from per-server `ServerConfig::request_timeout()` (`config.rs`, default 60s) |
| 70 | On timeout, sender SHOULD issue a cancellation notification | SHOULD | ✅ | rmcp `RequestHandle::await_response` auto-emits `CancelledNotification` (`reason="request timeout"`) on `PeerRequestOptions.timeout` expiry (`service.rs:321-347`); inherited at both sites via the option-bearing path |
| 71 | SDKs/middleware SHOULD allow per-request timeout configuration | SHOULD | ✅ | `request_timeout_secs` per-server field on `ServerConfig::Stdio` / `ServerConfig::Http` (`config.rs`, `#[serde(default = "default_request_timeout_secs")]` = 60), surfaced via `ServerConfig::request_timeout()` + `McpRuntimeManager::request_timeout()` accessor |
| 72 | MAY reset timeout clock on progress notification | MAY | N/A | no timeout to reset |
| 73 | Implementations SHOULD always enforce a maximum timeout (even with progress) | SHOULD | ✅ | every `tools/call` / `tools/list` is bounded by `PeerRequestOptions.timeout` (default 60s, no progress-based reset) — a hard ceiling at both `meta_tool.rs` sites |
| 74 | Implementations SHOULD handle version mismatch, capability failures, timeouts | SHOULD | ⚠️ | partial: version mismatch ✅ via rmcp `serve()` returning `Err` + our `with_context` wrap at `runtime.rs:1068,1081` → `ServerStatus::Failed`; timeout now ✅ (rows 69-71 implemented); capability failure ❌ (row 65, no gating) remains the only gap |
| 74a | `Implementation` object (clientInfo / serverInfo) carries optional `title`, `description`, `icons`, `websiteUrl` fields | (schema) | ✅ | `rmcp::model::Implementation` (1.7.0 schema) carries these via `serde(skip_serializing_if = "Option::is_none")`; we use bare default `Implementation` via `()` handler so we don't populate them — but the field-presence requirement is rmcp's responsibility (SDK side). Jelly fact-check 2026-06-03 filled |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [x] **Row 51 (HTTP `MCP-Protocol-Version` header)** — verified 2026-06-04: rmcp's worker injects the negotiated `mcp-protocol-version` into `protocol_headers` (SDK `streamable_http_client.rs:508-522`) and applies it to every post-handshake POST (`:542, :659`). No app-side injection needed; status ✅.
  - **Eval**: rmcp already handles this (Section 2 audit confirmed `protocol_headers` injection in SDK `transport/streamable_http_client.rs:408-418, 511-522`) · docs only · **fit: in-scope**. Just record the verification in the alignment doc + a one-line note in `runtime.rs` callsite; no code change.
- [ ] **Rows 52-56 (Client capabilities)**: decide minimum viable client capability set. Priority: (a) `roots` for filesystem-style servers that constrain by directory (most common server need); (b) `sampling` if/when we want servers to delegate LLM calls back to us; (c) `elicitation` for interactive UX. Implement via rmcp `ClientHandler` trait instead of `()`.
  - **Eval**: rmcp `ClientHandler` trait fully supports these (the unit `()` impl is a deliberate "no capabilities" sentinel) · drop-in for `roots` (wrapper struct + 1 method to expose `ServerConfig`-declared roots, ~80 LOC) · architectural commitment for `sampling`/`elicitation` (would need to wire back to the host LLM + ACP prompt UI) · **fit: in-scope for `roots`, borderline for `sampling`/`elicitation`**. Recommend ship `roots` first; defer the other two until a real server use case asks.
- [ ] **Row 65 (Capability gating)**: before calling `peer.list_all_tools()` / `peer.call_tool()`, inspect `peer.peer_info()?.capabilities.tools` — if absent, fail with a clear `ServerStatus::Failed("server does not advertise tools capability")` instead of letting rmcp surface a generic JSON-RPC error.
  - **Eval**: openab-agent only (rmcp exposes `peer.peer_info()` returning the cached `InitializeResult`) · drop-in (~20 LOC at `meta_tool.rs` boundary) · **fit: in-scope**. Cheap defensive check, better error message; aligns with our `ServerStatus` model.
- [~] **Row 66 (stdio shutdown ladder)** — *partial*: `McpRuntimeManager::disconnect()` (`runtime.rs:267`) now drives teardown via `cancellation_token().cancel()` → rmcp `graceful_shutdown` (close stdin → 3s grace → SIGKILL). The original 4-step ladder (explicit SIGTERM rung + tunable 5s+5s grace + `await` on child reap) is **not** fully reachable: rmcp's `graceful_shutdown` has no SIGTERM step and a fixed 3s grace, and the shared `Arc<RunningService>` blocks the owned/`&mut` `close()`/`cancel()` paths. Closing the SHOULD gap fully needs an rmcp upstream change; tracked as residual.
  - **Eval**: rmcp `TokioChildProcess` does NOT expose stdin-close-then-graceful-wait ladder — it relies on `Drop` which is ungraceful (SDK `transport/child_process.rs:23-200`) · hybrid · non-trivial (openab-agent wrapper ~60 LOC OR upstream rmcp PR adding `shutdown(grace)` method) · **fit: in-scope**. Spec is SHOULD, not MUST, but rude `SIGKILL`-on-drop has cost servers their state in the field. Wrapper is cleaner than waiting on upstream.
- [x] **Rows 69-71 (Request timeouts)** — DONE: added `request_timeout_secs` per-server field on `ServerConfig::Stdio` / `ServerConfig::Http` (`config.rs`, default 60s via `default_request_timeout_secs()`) + `ServerConfig::request_timeout()` accessor + `McpRuntimeManager::request_timeout(name)`. Both request sites (`call_tool`, `fetch_tools` in `meta_tool.rs`) switched from `peer.call_tool()` / `peer.list_all_tools()` to `send_request_with_option(req, PeerRequestOptions { timeout })` → `await_response()`.
  - **Eval**: openab-agent only · drop-in (~40 LOC config field + 2 callsite wraps) · **fit: in-scope**. Pure tokio idiom, no rmcp involvement; complements existing `breaker.rs` failure-rate logic with per-request bound.
- [x] **Row 70 (Cancellation notification on timeout)** — DONE: inherited for free by the rows 69-71 switch to `await_response()`, which auto-emits `CancelledNotification` (`reason="request timeout"`) on timeout. Added `tracing::info!(target:"mcp.cancel", …)` at both sites on `ServiceError::Timeout`. (`acp.rs:91-92` `session/cancel` TODO is a separate ACP surface, still open.)
  - **Eval (corrected 2026-06-03)**: openab-agent only (rmcp 1.7.0 `RequestHandle::await_response` (SDK `service.rs:322-343`) ALREADY auto-emits `CancelledNotification` with `reason="request timeout"` when `PeerRequestOptions.timeout` expires — request-id threading is internal to rmcp) · drop-in (~30-50 LOC: switch `peer.call_tool(p).await` to `peer.send_request_with_option(req, opt_with_timeout).await?.await_response().await`, unified with rows 69-71) · **fit: in-scope**. Prior eval (~120 LOC, non-trivial) was wrong — rmcp ships this pattern. See Section 4 Improvement Plan for consolidated treatment.

## Transports

Source: [`basic/transports.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 75 | JSON-RPC messages MUST be UTF-8 | MUST | ✅ | `serde_json` produces UTF-8; rmcp `AsyncRwTransport` (SDK `rmcp::transport::async_rw`) serializes via `serde_json::to_string` |
| 76 | Clients SHOULD support stdio whenever possible | SHOULD | ✅ | `Dial::Stdio` via `rmcp::transport::TokioChildProcess` (`src/mcp/runtime.rs:1064`); enabled via Cargo feature `transport-child-process` (`Cargo.toml:26`) |
| 77 | stdio messages delimited by newlines; MUST NOT contain embedded newlines | MUST NOT | ✅ | rmcp `AsyncRwTransport` uses newline-delimited framing (`rmcp::transport::async_rw` LinesCodec-style reader/writer); `serde_json::to_string` produces single-line JSON (no embedded `\n`) |
| 78 | Server MAY write UTF-8 to stderr for any logging (including non-error) | MAY | N/A | server-side |
| 79 | Client MAY capture/forward/ignore server's stderr | MAY | ✅ | `Dial::run` builds the stdio transport via `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()` (rmcp's `new()` defaults to `Stdio::inherit()`), takes the returned `Option<ChildStderr>`, and spawns a reader task that tees each line into `tracing::warn!(server=%name, "mcp stderr: …")` — `runtime.rs:1101` (Row 79). Operators now see `npx mcp-server-*` startup failures tagged per server instead of lost in container stderr |
| 80 | Client SHOULD NOT assume stderr indicates errors | SHOULD NOT | ✅ | we never read child stderr (inherited), so we don't classify it as an error signal |
| 81 | Server MUST NOT write non-MCP to stdout | MUST NOT | N/A | server-side |
| 82 | Client MUST NOT write non-MCP to server's stdin | MUST NOT | ✅ | rmcp `AsyncRwTransport` only writes serialized JSON-RPC frames; no other writes to child stdin |
| 83 | Streamable HTTP server MUST provide single endpoint supporting POST + GET | MUST | N/A | server-side |
| 84 | Server MUST validate `Origin` header on all incoming connections (DNS rebinding defence) | MUST | N/A | server-side |
| 85 | If Origin header is present and invalid, server MUST respond with HTTP 403 Forbidden | MUST | N/A | server-side |
| 85a | The 403 Forbidden response body MAY comprise a JSON-RPC error response with no `id` | MAY | N/A | server-side |
| 86 | Local servers SHOULD bind only to localhost (127.0.0.1), not all network interfaces (0.0.0.0) | SHOULD | N/A | server-side |
| 87 | Servers SHOULD implement proper authentication on all connections | SHOULD | N/A | server-side |
| 88 | Every client JSON-RPC message MUST be a new HTTP POST | MUST | ✅ | rmcp `StreamableHttpClient::post_message` per outbound message (SDK `transport/common/reqwest/streamable_http_client.rs:115`); driven by worker loop (SDK `transport/streamable_http_client.rs:441+`) |
| 89 | Client MUST use HTTP POST to send messages to the MCP endpoint | MUST | ✅ | `reqwest::Client::post(uri)` (SDK `transport/common/reqwest/streamable_http_client.rs:124`) |
| 90 | Client MUST include `Accept: application/json, text/event-stream` | MUST | ✅ | `[EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", ")` on POST + GET (SDK `transport/common/reqwest/streamable_http_client.rs:59, 125`) — both media types present (order reversed but RFC 7231 allows) |
| 91 | POST body MUST be a single JSON-RPC request, notification, or response | MUST | ✅ | `request.json(&message)` posts single `ClientJsonRpcMessage` (SDK `transport/common/reqwest/streamable_http_client.rs:135`); no batching |
| 92 | Server MUST return HTTP 202 Accepted (no body) on accepted notification/response input | MUST | N/A | server-side. Client accepts both `ACCEPTED` and `NO_CONTENT` as success (SDK `transport/common/reqwest/streamable_http_client.rs:168-172`) |
| 93 | If notification/response input is rejected, server MUST return an HTTP error status (e.g., 400 Bad Request) | MUST | N/A | server-side. Client surfaces non-2xx as `UnexpectedServerResponse` or parsed JSON-RPC error (SDK `transport/common/reqwest/streamable_http_client.rs:188-208`) |
| 94 | Error response body MAY be a JSON-RPC error response with no `id` | MAY | ✅ (client side) | `parse_json_rpc_error` accepts `JsonRpcMessage::Error` and surfaces it (SDK `transport/common/reqwest/streamable_http_client.rs:39-44, 197-204`) |
| 95 | For JSON-RPC request input, server MUST return either `Content-Type: text/event-stream` (SSE stream) or `Content-Type: application/json` (single JSON object) | MUST | N/A | server-side |
| 96 | Client MUST support both SSE and JSON response content types | MUST | ✅ | content-type branch in SDK `transport/common/reqwest/streamable_http_client.rs:210-234` handles both `EVENT_STREAM_MIME_TYPE` (→ `SseStream::from_byte_stream`) and `JSON_MIME_TYPE` (→ `response.json::<ServerJsonRpcMessage>`); unknown → `UnexpectedContentType` |
| 97 | On SSE initiation, server SHOULD immediately send event with ID + empty `data` to prime reconnection | SHOULD | N/A | server-side |
| 98 | After event-ID-bearing SSE event, server MAY close connection (without terminating SSE stream) | MAY | N/A | server-side |
| 99 | Client SHOULD poll SSE stream by reconnecting when server closes connection | SHOULD | ✅ | rmcp `SseRetryPolicy` + `retry_connection(last_event_id)` (SDK `transport/common/client_side_sse.rs:99, 112`); default `ExponentialBackoff` (SDK `transport/streamable_http_client.rs:1147`) |
| 100 | If server closes connection before terminating SSE stream, it SHOULD send a `retry` SSE field | SHOULD | N/A | server-side |
| 101 | Client MUST respect SSE `retry` field, waiting that many ms before reconnect | MUST | ⚠️ | Verified 2026-06-04: rmcp parses the `retry:` field into `server_retry_interval` (SDK `client_side_sse.rs:211-214`) and on the **graceful-close** reconnect path it overrides the policy outright — `server_retry_interval.take().or_else(\|\| retry_policy.retry(0))` (`:265-268`) ✅. But on the **error** reconnect path it combines via `max(server_retry, policy_interval)` (`:292-296`), so a configured backoff larger than the server's `retry` wins and the policy can still terminate retries — strictly violates "MUST override". Mitigation: a small `FixedInterval` policy makes `max()` pick the server value. Full fix needs an rmcp upstream change to `:292-296`; tracked as residual soft-gap |
| 102 | SSE stream SHOULD eventually include the JSON-RPC response for the originating request | SHOULD | N/A | server-side |
| 103 | Server MAY send other requests/notifications on SSE before the response | MAY | N/A | server-side |
| 104 | Pre-response messages SHOULD relate to originating request | SHOULD | N/A | server-side |
| 105 | Server MAY terminate SSE stream if session expires | MAY | N/A | server-side |
| 106 | After response sent, server SHOULD terminate SSE stream | SHOULD | N/A | server-side |
| 107 | Disconnection MAY occur at any time | MAY | N/A | observational |
| 108 | Disconnection SHOULD NOT be interpreted as request cancellation | SHOULD NOT | ✅ | rmcp client treats SSE disconnect as a transient stream event → triggers reconnect (`retry_connection`), not request abort |
| 109 | To cancel, client SHOULD send `CancelledNotification` | SHOULD | ✅ | timeout path now sends it: `await_response()` auto-emits `CancelledNotification` (`reason="request timeout"`) on expiry at both `meta_tool.rs` request sites (`service.rs:321-347`). Explicit user-driven `session/cancel` (`acp.rs:91-92`) is a separate ACP surface, still open |
| 110 | Server MAY make stream resumable to avoid message loss on disconnect | MAY | N/A | server-side |
| 111 | Client MAY issue HTTP GET to open SSE listening stream | MAY | ✅ | rmcp `StreamableHttpClient::get_stream` (SDK `transport/common/reqwest/streamable_http_client.rs:49-89`) — invoked by worker when session permits server-initiated traffic |
| 112 | GET MUST include `Accept: text/event-stream` | MUST | ✅ | `Accept: text/event-stream, application/json` on GET (SDK `transport/common/reqwest/streamable_http_client.rs:59`) |
| 113 | On GET, server MUST return `Content-Type: text/event-stream` or HTTP 405 Method Not Allowed (indicating no SSE at this endpoint) | MUST | N/A (client handles both) | client maps 405 → `ServerDoesNotSupportSse` and proceeds without GET stream (SDK `transport/common/reqwest/streamable_http_client.rs:69-71`) |
| 114 | Server MAY send JSON-RPC requests/notifications on GET SSE stream | MAY | N/A | server-side |
| 115 | GET-stream messages SHOULD be unrelated to concurrent client requests | SHOULD | N/A | server-side |
| 116 | Server MUST NOT send a JSON-RPC response on GET stream unless resuming a previous request | MUST NOT | N/A | server-side |
| 117 | Server MAY close GET SSE stream at any time | MAY | N/A | server-side |
| 118 | If server closes GET connection without terminating stream, it SHOULD send `retry` (same polling behavior) | SHOULD | N/A | server-side; client side mirrors row 99/101 handling |
| 119 | Client MAY close SSE stream at any time | MAY | ✅ | rmcp `WorkerQuitReason` / cancellation token drops the SSE stream (SDK `transport/streamable_http_client.rs:464-465`) |
| 120 | Client MAY remain connected to multiple SSE streams simultaneously | MAY | ⚠️ | rmcp worker typically holds POST-response SSE + GET SSE concurrently; not explicitly multiplexed beyond that. Likely acceptable for our single-server-per-`ServerHandle` model |
| 121 | Server MUST send each JSON-RPC message on only one stream (no broadcasting) | MUST | N/A | server-side |
| 122 | Servers MAY attach `id` to SSE events for resumability | MAY | N/A | server-side |
| 123 | If present, SSE event ID MUST be globally unique across all streams within the session (or across all streams for that client if session management is not in use) | MUST | N/A | server-side |
| 124 | Event IDs SHOULD encode sufficient info to identify the originating stream | SHOULD | N/A | server-side |
| 125 | To resume after disconnect, client SHOULD issue HTTP GET with `Last-Event-ID` header (regardless of original transport) | SHOULD | ✅ | `get_stream` accepts `last_event_id: Option<String>` and sets `HEADER_LAST_EVENT_ID` (SDK `transport/common/reqwest/streamable_http_client.rs:53, 61-63`); `SseRetryPolicy::retry_connection(last_event_id)` carries last id (SDK `transport/common/client_side_sse.rs:112`) |
| 126 | Server MAY replay messages from `Last-Event-ID` on the disconnected stream | MAY | N/A | server-side |
| 127 | Server MUST NOT replay messages from a different stream | MUST NOT | N/A | server-side |
| 128 | Server MAY assign session ID at initialization by including `MCP-Session-Id` header on the HTTP response containing the `InitializeResult` | MAY | ✅ (client extracts) | rmcp worker reads `HEADER_SESSION_ID` from init POST response → `session_id: Option<Arc<str>>` (SDK `transport/streamable_http_client.rs:497, 181-185`) |
| 129 | Session ID SHOULD be globally unique and cryptographically secure | SHOULD | N/A | server-side |
| 130 | Session ID MUST only contain visible ASCII (0x21–0x7E) | MUST | N/A | server-side; client passes raw header value through `HeaderValue` (rejects non-ASCII at the http crate level) |
| 131 | Client MUST handle session ID securely | MUST | ✅ | session id kept in `Arc<str>` inside worker (SDK `transport/streamable_http_client.rs:497`); never logged at our layer; not persisted to disk |
| 132 | Client MUST include `MCP-Session-Id` on all subsequent HTTP requests when issued | MUST | ✅ | POST attaches `HEADER_SESSION_ID` when `session_id.is_some()` (SDK `transport/common/reqwest/streamable_http_client.rs:131-134`); GET + DELETE always attach (lines 60, 102) |
| 133 | Servers requiring a session SHOULD respond HTTP 400 to non-init requests without `MCP-Session-Id` | SHOULD | N/A | server-side. Client side: rmcp default `allow_stateless = true` (SDK `transport/streamable_http_client.rs:1149`) tolerates servers that don't issue a session id; servers that do require it will hand back 400 which we surface as `UnexpectedServerResponse` |
| 134 | Server MAY terminate session at any time | MAY | N/A | server-side |
| 135 | Post-termination, server MUST respond HTTP 404 to requests with that session ID | MUST | ✅ (client handles) | rmcp maps `NOT_FOUND` + `session_was_attached` → `StreamableHttpError::SessionExpired` (SDK `transport/common/reqwest/streamable_http_client.rs:174-176`) |
| 136 | On HTTP 404 with session ID, client MUST start a new session via fresh `InitializeRequest` (no session ID) | MUST | ✅ | rmcp `perform_reinitialization` re-POSTs saved init request with `session_id: None`, then resumes (SDK `transport/streamable_http_client.rs:386-438`) |
| 137 | Client SHOULD send HTTP DELETE with `MCP-Session-Id` to terminate session | SHOULD | ✅ | `delete_session` sends DELETE + `HEADER_SESSION_ID` (SDK `transport/common/reqwest/streamable_http_client.rs:91-113`); rmcp `SessionCleanupInfo` triggers it on worker shutdown (SDK `transport/streamable_http_client.rs:524-531`) |
| 138 | Server MAY return HTTP 405 to DELETE | MAY | ✅ (client tolerates) | rmcp treats 405 on DELETE as success (SDK `transport/common/reqwest/streamable_http_client.rs:107-110`) with `tracing::debug!("this server doesn't support deleting session")` |
| 139 | Client MUST include `MCP-Protocol-Version: <protocol-version>` header on all HTTP requests | MUST | ✅ | rmcp worker injects the negotiated `mcp-protocol-version` into `protocol_headers` (SDK `transport/streamable_http_client.rs:508-522`) and threads that same map through **both** every post-handshake POST (`:542, :659`) **and** the standalone GET stream + its reconnects (`spawn_headers = protocol_headers.clone()` → `get_stream(...)`, `:570, :579, :591, :715, :727`). GET path verified 2026-06-04 — no separate `custom_headers` gap. openab-agent sets no headers itself (`runtime.rs:1113-1121`), relying on the SDK |
| 140 | Sent protocol-version header value SHOULD be the negotiated one | SHOULD | ✅ | uses `init_result.protocol_version.as_str()` directly (SDK `transport/streamable_http_client.rs:413, 515`) |
| 141 | If server receives no `MCP-Protocol-Version` header and has no other way to identify the version (e.g., via initialization negotiation), it SHOULD assume `2025-03-26` | SHOULD | N/A | server-side |
| 142 | If invalid/unsupported `MCP-Protocol-Version` is sent, server MUST respond HTTP 400 | MUST | N/A | server-side; rmcp surfaces as `UnexpectedServerResponse` |
| 143 | Implementations MAY implement custom transports | MAY | N/A | we don't implement custom transports — stdio + Streamable HTTP only (`src/mcp/runtime.rs:1042-1053`) |
| 144 | Custom transports MUST preserve JSON-RPC + lifecycle | MUST | N/A | no custom transports |
| 145 | Custom transports SHOULD document connection establishment / message exchange patterns | SHOULD | N/A | no custom transports |
| 145a | Client MAY implement legacy HTTP+SSE backwards-compat flow: POST `InitializeRequest`; on HTTP 400/404/405 fall back to GET expecting `endpoint` SSE event (for interop with 2024-11-05 HTTP+SSE servers) | MAY | N/A (intentional) | conscious decision (Brett 2026-06-03) to **not** implement legacy 2024-11-05 HTTP+SSE compatibility. rmcp 1.7.0 client doesn't fall back; init failure against a legacy-only server surfaces as `UnexpectedServerResponse`. Servers MUST upgrade to Streamable HTTP to be supported |
| 145b | Servers wanting to support older clients SHOULD continue to host both the SSE and POST endpoints of the old transport, alongside the new MCP endpoint | SHOULD | N/A | server-side |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [x] **Row 79 (stderr capture)** — implemented in `Dial::run` (`runtime.rs:1101`): `TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()` returns `(transport, Option<ChildStderr>)`; we spawn a reader task teeing each line to `tracing::warn!(server=%name, "mcp stderr: …")`. Note: the planned `TokioChildProcess::stderr(...)` accessor does not exist in rmcp 1.7.0 (`new()` inherits stderr; the builder `.stderr()` + `.spawn()` tuple is the only capture path) — used the field-based `server=%name` convention rather than `target=` to match the module.
  - **Eval**: rmcp `TokioChildProcess::builder()` exposes the `stderr(Stdio)` setter (SDK `transport/child_process.rs`), so the plumbing exists · openab-agent drop-in (~50 LOC: pipe stderr, spawn tokio task that reads lines + emits `tracing::info!(target=..., line=%line)`) · **fit: in-scope**. Operator-quality-of-life win; matches our existing tracing-only observability rule.
- [ ] **Row 90 (`Accept` header order)**: cosmetic — spec lists `application/json, text/event-stream`, rmcp emits `text/event-stream, application/json`. Order is non-normative per RFC 7231; document this in alignment doc as acceptable rather than file an rmcp PR.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Already noted in row 90 cell. No code change needed; RFC 7231 §5.3.2 makes order non-significant for Accept.
- [~] **Row 101 (SSE `retry` field honoured)** — verified 2026-06-04, *partial*: rmcp parses `retry:` → `server_retry_interval` (`client_side_sse.rs:211-214`) and on the graceful-close reconnect it overrides the policy (`:265-268`) ✅; but on the error reconnect it uses `max(server_retry, policy_interval)` (`:292-296`), so a larger configured backoff wins and the policy can still terminate retries — strictly violates the MUST. Mitigation: small `FixedInterval` policy. Full fix = rmcp upstream change to `:292-296`; tracked as residual soft-gap (cannot be closed from openab-agent config alone).
  - **Eval**: rmcp upstream (the retry-loop scheduler lives in SDK `transport/common/client_side_sse.rs`) · non-trivial (investigation first ~30 min reading, then either zero code if honoured or upstream rmcp PR if not) · **fit: in-scope — investigation**. We can't easily wrapper around this — it's deep in the SSE reconnect loop. If gap confirmed, upstream is the right place.
- [x] **Row 109 (CancelledNotification)** — DONE (timeout path): `notifications/cancelled` now emitted on request-timeout at both `meta_tool.rs` sites via the rows 69-71 `await_response()` switch (shared implementation). Explicit `session/cancel` ACP routing (`acp.rs:91-92`) remains a separate open item.
  - **Eval**: dup of Section 1 Row 70 eval — openab-agent only (rmcp `Peer::notify_cancelled` exists but request-id threading is non-trivial via the same `send_request_with_option`/`await_response` path used in Section 1) · drop-in (~30-50 LOC, shared with Section 1 Row 70) · **fit: in-scope — sequence after rows 69-71**. Single implementation covers both rows.
- [x] **Row 139 (GET stream protocol header)** — verified 2026-06-04: no gap. The worker clones `protocol_headers` into `spawn_headers` and passes it to the standalone `get_stream(...)` (SDK `streamable_http_client.rs:570, :579`); reconnects keep it via `custom_headers: spawn_headers` (`:591`) and the re-init GET path repeats it (`:715, :727`). The negotiated `mcp-protocol-version` therefore rides every GET. Status ✅.
  - **Eval**: hybrid (header-merge happens in SDK `transport/streamable_http_client.rs` worker plumbing; workaround can live openab-agent-side) · non-trivial (investigation first; then either zero code, upstream rmcp PR, or openab-agent custom-headers workaround ~20 LOC) · **fit: in-scope — investigation**. This is a real MUST so worth the dig; if gap confirmed, upstream PR preferable but `custom_headers` passthrough is a viable workaround until merged.

## Authorization

Source: [`basic/authorization.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/authorization.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 146 | Authorization is OPTIONAL | OPTIONAL | ✅ | per-server `oauth: Option<OAuthConfig>` (`src/mcp/config.rs:33`); HTTP servers can run anonymous (`runtime.rs:1077`) |
| 147 | HTTP transports SHOULD conform to authorization spec | SHOULD | ⚠️ | OAuth 2.1 PKCE flow + bearer header (`src/mcp/oauth.rs`, `src/mcp/flow.rs`, `auth.rs:351-410`) — base mechanics conform; RFC 8707 resource param now landed for custom providers (rows 183-190); remaining MCP-spec gaps in PRM (RFC 9728), AS-metadata discovery dual-mechanism (RFC 8414+OIDC), CIMD, built-in audience binding — see rows below |
| 148 | STDIO SHOULD NOT follow this spec; credentials from environment | SHOULD NOT | ✅ | stdio uses `env_clear()` + explicit `envs(stdio_child_env(&env))` (`src/mcp/runtime.rs:1059-1063`); no OAuth on stdio path |
| 149 | Alternative transports MUST follow established security best practices for their protocol | MUST | N/A | no alternative transports beyond stdio + Streamable HTTP |
| 150 | Authorization servers MUST implement OAuth 2.1 with appropriate security measures for both confidential and public clients | MUST | N/A | AS-side |
| 151 | AS and MCP clients SHOULD support OAuth Client ID Metadata Documents | SHOULD | ❌ | no CIMD client support; we use pre-registered client IDs only (env-injected via `OPENAB_MCP_<provider>_CLIENT_ID`, `oauth.rs:53-69` for the `builtin_client_id` impl). Jelly fact-check 2026-06-03 repointed from stale `oauth.rs:188-209` (unit-test lines) |
| 152 | AS and MCP clients MAY support RFC 7591 Dynamic Client Registration | MAY | ❌ | no DCR implementation |
| 153 | MCP servers MUST implement RFC 9728 Protected Resource Metadata | MUST | N/A | server-side |
| 154 | MCP clients MUST use RFC 9728 PRM for authorization server discovery | MUST | ❌ | no PRM consumer; AS endpoints come from built-in `ProviderSpec` (`oauth.rs:14-30`) or custom `OAuthConfig.{authorize_url, token_url}` (`config.rs:81-83`). `discovery: bool` field (`config.rs:95`) is declared but per `oauth.rs::resolve_custom` the discovery branch is not actually exercised — `authorize_url` + `token_url` are required even when `discovery=true` |
| 155 | AS MUST provide at least one of: RFC 8414 AS metadata, OpenID Connect Discovery 1.0 | MUST | N/A | AS-side |
| 156 | MCP clients MUST support both AS metadata discovery mechanisms (RFC 8414 + OIDC Discovery 1.0) | MUST | ❌ | no AS-metadata discovery client; `discovery_allowlist` SSRF guard (`config.rs:104-110`) is scaffolding for the missing implementation |
| 157 | PRM document MUST include `authorization_servers` field with ≥1 AS | MUST | N/A | server-side |
| 158 | MCP servers MUST implement one of the following PRM discovery mechanisms: `resource_metadata` in `WWW-Authenticate` on 401, or RFC 9728 well-known URI | MUST | N/A | server-side |
| 159 | MCP clients MUST support both PRM discovery mechanisms (header + well-known fallback) | MUST | ❌ | no `WWW-Authenticate` parse, no `.well-known/oauth-protected-resource` fetch — searched `src/`: zero matches for `WWW-Authenticate` / `resource_metadata` / `protected-resource` outside `Cargo.lock` traces |
| 160 | MCP servers SHOULD include `scope` in `WWW-Authenticate` (per RFC 6750 §3) | SHOULD | N/A | server-side |
| 161 | Clients MUST NOT assume relationship between `WWW-Authenticate` scope set and `scopes_supported` | MUST NOT | ❌ (vacuously) | we don't parse `WWW-Authenticate` at all, so we don't conflate — but we also don't honour it; net gap |
| 162 | Clients MUST treat challenge-provided scopes as authoritative for current request | MUST | ❌ | no challenge parsing |
| 163 | Servers SHOULD strive for consistency in scope set construction | SHOULD | N/A | server-side |
| 164 | MCP clients MUST be able to parse `WWW-Authenticate` headers and respond appropriately to 401 | MUST | ❌ | rmcp 1.7.0 surfaces 401 as `StreamableHttpError::AuthRequired` carrying the raw `WWW-Authenticate` header value (`rmcp transport/common/reqwest/streamable_http_client.rs:136-149` SDK) — but our agent code does not parse it or trigger reauth; just propagates the error |
| 165 | If `scope` is absent from `WWW-Authenticate`, clients SHOULD apply Scope Selection Strategy fallback | SHOULD | ❌ | no Scope Selection Strategy implementation |
| 166 | Clients MUST attempt multiple well-known endpoints (RFC 8414 + OIDC) when discovering AS metadata | MUST | ❌ | no discovery path |
| 167 | For path-bearing issuer URLs, clients MUST try priority order: oauth-authorization-server path-insert, openid-configuration path-insert, openid-configuration appended | MUST | ❌ | no discovery path |
| 168 | For pathless issuer URLs, clients MUST try oauth-authorization-server, then openid-configuration | MUST | ❌ | no discovery path |
| 169 | Clients supporting all registration options SHOULD prefer pre-registered, then CIMD, then DCR, then prompt | SHOULD | ⚠️ (vacuously) | we only support pre-registered (env-injected client IDs) — top of the priority list is honoured; the rest don't exist |
| 170 | MCP clients and AS SHOULD support OAuth Client ID Metadata Documents | SHOULD | ❌ | duplicate of row 151; no CIMD |
| 171 | CIMD-supporting MCP implementations MUST follow OAuth CIMD requirements | MUST | N/A | no CIMD support — vacuously satisfied |
| 172 | CIMD: clients MUST host metadata document at HTTPS URL per RFC requirements | MUST | N/A | no CIMD |
| 173 | CIMD: `client_id` URL MUST use `https` scheme with a path component | MUST | N/A | no CIMD |
| 174 | CIMD: metadata MUST include at least `client_id`, `client_name`, `redirect_uris` | MUST | N/A | no CIMD |
| 175 | CIMD: clients MUST ensure `client_id` value matches the document URL exactly | MUST | N/A | no CIMD |
| 176 | CIMD: clients MAY use `private_key_jwt` for client authentication | MAY | N/A | no CIMD |
| 177 | CIMD: MCP clients SHOULD check for `client_id_metadata_document_supported` AS capability | SHOULD | N/A | no CIMD |
| 178 | CIMD: MCP clients MAY fall back to DCR or pre-registration if CIMD unavailable | MAY | ⚠️ | we _always_ use pre-registration; this is "fall back to" by virtue of having no CIMD or DCR — vacuously satisfied |
| 178a | CIMD (AS-side): AS SHOULD fetch metadata documents when encountering URL-formatted `client_id`s | SHOULD | N/A — client-side | (AS-side) |
| 178b | CIMD (AS-side): AS MUST validate fetched document's `client_id` matches the URL exactly | MUST | N/A — client-side | (AS-side) |
| 178c | CIMD (AS-side): AS SHOULD cache metadata respecting HTTP cache headers | SHOULD | N/A — client-side | (AS-side) |
| 178d | CIMD (AS-side): AS MUST validate redirect URIs in authorization request against metadata document | MUST | N/A — client-side | (AS-side) |
| 178e | CIMD (AS-side): AS MUST validate metadata document structure is valid JSON and contains required fields | MUST | N/A — client-side | (AS-side) |
| 179 | Pre-registration: MCP clients SHOULD support an option for static client credentials | SHOULD | ✅ | env-injected client ID per built-in provider (`oauth.rs::builtin_client_id`, env var pattern `OPENAB_MCP_<provider>_CLIENT_ID`); custom providers carry `client_id: Option<String>` on `OAuthConfig` (`config.rs:85`) |
| 180 | MCP clients and AS MAY support RFC 7591 Dynamic Client Registration | MAY | ❌ | no DCR |
| 181 | Scope Selection: clients SHOULD follow least privilege when requesting scopes | SHOULD | ⚠️ | per-built-in `default_scopes` baked in (`oauth.rs::ProviderSpec`); custom providers carry user-supplied `scopes` (`config.rs:79`) — no enforcement that the set is least-privilege, but defaults are deliberately minimal (e.g. Linear `read`-set) |
| 182 | Scope Selection: clients SHOULD prefer `scope` from initial `WWW-Authenticate` header, else `scopes_supported` from PRM, else omit `scope` | SHOULD | ❌ | no challenge-driven scope selection |
| 183 | MCP clients MUST implement RFC 8707 Resource Indicators (`resource` parameter) | MUST | ⚠️ | implemented for **custom** providers: authorize URL via `flow.rs::init_paste_authorize` (`resource: Option<&str>` → `append_pair("resource", ...)`), token/refresh/device requests via `runtime.rs::post_token_exchange`/`post_token_refresh`/`post_device_token_poll`/`post_device_authorization`. **Built-in** Anthropic intentionally omits (gated in `runtime.rs::resolve_paste_client`/`resolve_device_client`) — see row 190 note. NB: the real MCP OAuth flow lives in `flow.rs` + `runtime.rs`, **not** `auth.rs` (legacy Codex/OpenAI flow); earlier doc callsite refs to `auth.rs:357/410/493/585` were wrong |
| 184 | `resource` parameter MUST be included in both authorization and token requests | MUST | ⚠️ | both carry it for custom providers (authorize URL + every token-endpoint POST incl. refresh + device poll); built-in gated off |
| 185 | `resource` parameter MUST identify the intended MCP server | MUST | ✅ | value = canonical form of `ServerConfig::Http.url` (the MCP server's own URL), computed in `flow.rs::canonical_resource` |
| 186 | `resource` MUST use the canonical URI per RFC 8707 §2 | MUST | ✅ | `flow.rs::canonical_resource` — lowercase scheme+host (url crate), drop default port, strip trailing slash, drop fragment, preserve query |
| 187 | MCP clients SHOULD provide the most specific URI possible for the MCP server | SHOULD | ✅ | full server URL including path retained (only fragment + trailing slash normalized away) |
| 188 | Implementations SHOULD accept uppercase scheme/host for robustness | SHOULD | ✅ | `canonical_resource` lowercases via `url::Url` parse (test: `canonical_resource_lowercases_and_strips_default_port_and_trailing_slash`) |
| 189 | Implementations SHOULD consistently use no-trailing-slash form for interoperability | SHOULD | ✅ | `canonical_resource` strips trailing slash (test: `canonical_resource_bare_host_has_no_trailing_slash`) |
| 190 | MCP clients MUST send `resource` regardless of AS support | MUST | ⚠️ | satisfied for custom providers; **built-in Anthropic deliberately gated off** — its authorize/token endpoints point at the vendor AS (claude.ai / platform.claude.com), not the MCP server's URL, and there's no evidence that AS honors `resource` (a real `invalid_target` would break the one shipping built-in login). Documented divergence pending PRM/discovery (rows 153-168) which would let the client learn the correct audience |
| 191 | Access token handling MUST conform to OAuth 2.1 §5 | MUST | ✅ | tokens stored on disk via `save_namespaced_token_at` (`auth.rs:212`), retrieved via `load_namespaced_token_at` (`auth.rs:197`); bearer-injected into transport via `auth_header(token)` (`src/mcp/runtime.rs:1073-1074`) |
| 192 | MCP client MUST use `Authorization: Bearer <access-token>` header | MUST | ✅ | rmcp `StreamableHttpClientTransportConfig::auth_header(token)` injects bearer (SDK `transport/common/reqwest/streamable_http_client.rs:126-128` uses `request.bearer_auth(...)`) |
| 193 | Authorization MUST be included on every HTTP request from client to server | MUST | ✅ | `auth_header` is part of `StreamableHttpClientTransportConfig`; rmcp re-applies on every POST + GET + DELETE (`transport/common/reqwest/streamable_http_client.rs:64-66, 99-101, 126-128`) |
| 194 | Access tokens MUST NOT be in URI query | MUST NOT | ✅ | rmcp always uses header (see row 192); no query usage in our code |
| 195 | MCP clients MUST NOT send tokens to the MCP server other than ones issued by the MCP server's AS | MUST NOT | ✅ | per-server `oauth` block resolves to per-server token store (`namespaced_token` keyed by server name); no cross-server token reuse |
| 196 | MCP servers MUST validate access tokens per OAuth 2.1 §5.2 | MUST | N/A | server-side |
| 197 | MCP servers MUST validate tokens were issued specifically for them (audience) | MUST | N/A | server-side |
| 198 | On validation failure, MCP servers MUST follow OAuth 2.1 §5.3 error handling | MUST | N/A | server-side |
| 199 | Invalid/expired tokens MUST receive HTTP 401 | MUST | N/A | server-side; client side: 401 surfaces as `AuthRequired` error (SDK) — see row 164 |
| 200 | MCP servers MUST only accept tokens valid for their own resources | MUST | N/A | server-side |
| 201 | MCP servers MUST NOT accept or transit any other tokens | MUST NOT | N/A | server-side |
| 202 | Servers MUST return appropriate HTTP status (401/403/400) for auth errors | MUST | N/A | server-side |
| 203 | On runtime insufficient scope, server SHOULD return 403 + `WWW-Authenticate` with `error="insufficient_scope"`, `scope`, `resource_metadata`, optional `error_description` | SHOULD | N/A | server-side; client surface: rmcp 403 path → `StreamableHttpError::InsufficientScope` carrying `required_scope` extracted from `WWW-Authenticate` (SDK `transport/common/reqwest/streamable_http_client.rs:151-166`) |
| 204 | On insufficient-scope error, servers SHOULD include required scopes in `scope` parameter | SHOULD | N/A | server-side |
| 205 | Servers SHOULD be consistent in scope inclusion strategy | SHOULD | N/A | server-side |
| 206 | Servers SHOULD consider UX impact when choosing scopes for insufficient-scope errors | SHOULD | N/A | server-side |
| 207 | Clients SHOULD respond to scope errors via step-up authorization flow OR handle the errors in other appropriate ways | SHOULD | ❌ | rmcp surfaces `InsufficientScope.required_scope` (SDK) but our agent does not consume it — error bubbles up; no step-up reauth |
| 207a | Clients acting on behalf of a user SHOULD attempt the step-up authorization flow | SHOULD | ❌ | as above |
| 208 | `client_credentials` clients MAY attempt step-up authorization or abort | MAY | N/A | we don't use client_credentials grant |
| 209 | Clients SHOULD implement retry limits and track scope-upgrade attempts | SHOULD | N/A | no step-up implementation to limit |
| 210 | Implementations MUST follow OAuth 2.1 §7 Security Considerations | MUST | ⚠️ | PKCE + state nonce ✅, RFC 8707 resource audience binding now landed for custom providers (rows 183-190); remaining gaps: built-in audience binding (gated, row 190), `WWW-Authenticate`-driven reauth (row 164) |
| 211 | MCP clients MUST include `resource` parameter for audience binding | MUST | ⚠️ | dup of 183/190 — implemented for custom providers, built-in gated (see row 183/190) |
| 212 | MCP servers MUST validate tokens are issued for their own use | MUST | N/A | server-side |
| 213 | Clients and servers MUST implement secure token storage per OAuth 2.1 §7.1 | MUST | ⚠️ | tokens written to `auth.json` under `~/.openab/agent/` via `auth.rs` (filesystem-permissioned to user); not encrypted at rest, not in OS keyring. Adequate for a single-user agent on a single-user host but not "secure storage" in the strong sense |
| 214 | AS SHOULD issue short-lived access tokens | SHOULD | N/A | AS-side |
| 215 | For public clients, AS MUST rotate refresh tokens | MUST | N/A | AS-side; client-side: refresh-token handling lives in `auth.rs` token-refresh path |
| 216 | Implementations MUST follow OAuth 2.1 §1.5 Communication Security | MUST | ✅ | all auth flow URLs are HTTPS in built-in `ProviderSpec`s (`oauth.rs:14-30`); custom providers have unenforced URL scheme — see Improvement Plan |
| 217 | All AS endpoints MUST be HTTPS | MUST | ✅ | `OAuthConfig::validate` (`config.rs:131`) rejects non-`https://` `authorize_url` / `token_url` for custom providers (`oauth::builtin(p).is_none()`); built-ins pin vetted URLs in `ProviderSpec`. Tests: `validate_rejects_http_authorize_url` / `validate_rejects_http_token_url` / `validate_accepts_https_urls` |
| 218 | All redirect URIs MUST be localhost or HTTPS | MUST | ✅ | `OAuthConfig::validate` rejects custom `redirect_uri` that is neither `https://` nor `http://` loopback (`is_loopback_or_https_redirect`, `config.rs`, RFC 8252 §7.3); built-ins pin `http://localhost:<port>/callback`. Tests: `validate_rejects_non_localhost_http_redirect_uri` / `validate_accepts_localhost_http_redirect_uri` |
| 219 | MCP clients MUST implement PKCE per OAuth 2.1 §7.5.2 | MUST | ✅ | `generate_pkce()` + `code_challenge_method=S256` in `src/mcp/flow.rs:42-50` (paste-back) and `src/auth.rs:351, 357` (browser); verifier preserved across paste-back via `PendingPasteLogin` (`runtime.rs:269-275`) and sent to token endpoint (`auth.rs:410, 493, 585`) |
| 220 | MCP clients MUST verify PKCE support before proceeding with authorization | MUST | ❌ | no `code_challenge_methods_supported` check; PKCE is unconditionally sent regardless of AS metadata (which we don't fetch — see row 156) |
| 221 | MCP clients MUST use `S256` code challenge method when technically capable (OAuth 2.1 §4.1.1) | MUST | ✅ | unconditional `S256` (`flow.rs:50`, `auth.rs:357`) |
| 222 | OAuth 2.0 AS metadata: if `code_challenge_methods_supported` absent, clients MUST refuse to proceed | MUST | ❌ | no AS-metadata fetch (row 156) so we can't enforce this; we send PKCE anyway, which is the safe behaviour but technically violates the "refuse to proceed" wording when metadata is absent |
| 223 | OIDC Discovery 1.0: clients MUST verify `code_challenge_methods_supported` is present; refuse if absent | MUST | ❌ | as above |
| 224 | AS providing OIDC Discovery 1.0 MUST include `code_challenge_methods_supported` | MUST | N/A | AS-side |
| 225 | MCP clients MUST have redirect URIs registered with the AS | MUST | ✅ | built-ins pin `callback` per `ProviderSpec` (`oauth.rs:18, 28`), pre-registered with each AS; custom flow requires `redirect_uri` field |
| 226 | AS MUST validate exact redirect URIs against pre-registered values | MUST | N/A | AS-side |
| 227 | MCP clients SHOULD use and verify `state` parameter, discard mismatches | SHOULD | ✅ | `state` generated + verified per `flow.rs::init_paste_authorize` (random nonce) + `flow.rs::parse_paste_callback` (state echo check per RFC 6749 §10.12, `flow.rs:82-84`; see runtime.rs:299 comment); state snapshot in `PendingPasteLogin` (`runtime.rs:271`) |
| 228 | AS MUST take precautions to prevent redirecting to untrusted URIs | MUST | N/A | AS-side |
| 229 | AS SHOULD only auto-redirect if URI is trusted | SHOULD | N/A | AS-side |
| 230 | AS implementing CIMD MUST consider security implications per CIMD §6 | MUST | N/A | AS-side / no CIMD |
| 231 | AS fetching CIMD documents SHOULD consider SSRF risks | SHOULD | N/A | AS-side |
| 232 | AS SHOULD display additional warnings for `localhost`-only redirect URIs | SHOULD | N/A | AS-side |
| 233 | AS MAY require additional attestation mechanisms for enhanced security (esp. in the context of `localhost` redirect URIs) | MAY | N/A | AS-side |
| 234 | AS MUST clearly display the redirect URI hostname during authorization | MUST | N/A | AS-side |
| 235 | AS MAY implement domain-based trust policies | MAY | N/A | AS-side |
| 236 | MCP proxies with static client IDs MUST obtain user consent for each dynamically registered client | MUST | N/A | we are not a proxy / multi-tenant AS |
| 237 | MCP servers MUST validate access tokens before processing requests | MUST | N/A | server-side |
| 238 | MCP servers MUST follow OAuth 2.1 §5.2 for token validation | MUST | N/A | server-side |
| 239 | MCP servers MUST only accept tokens specifically intended for themselves | MUST | N/A | server-side |
| 240 | MCP servers MUST reject tokens that do not include them in the audience claim, or otherwise verify they are the intended recipient | MUST | N/A | server-side |
| 241 | MCP servers MUST NOT pass through MCP-client tokens to upstream APIs | MUST NOT | N/A | server-side |
| 242 | MCP clients MUST implement and use the RFC 8707 `resource` parameter (aligns with RFC 9728 §7.4 recommendation) | MUST | ⚠️ | dup of 183-190 — implemented for custom providers via `flow.rs::canonical_resource` + `init_paste_authorize` + `runtime.rs` token POSTs; built-in gated (see row 190) |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Rows 153, 154, 156, 159, 164-168 (PRM + AS-metadata discovery)**: implement the spec-required client discovery surface — (a) on initial 401, parse `WWW-Authenticate` for `resource_metadata` and `scope`; (b) GET `/.well-known/oauth-protected-resource{path}` to fetch PRM; (c) follow `authorization_servers` to RFC 8414 / OIDC `.well-known/oauth-authorization-server` + `openid-configuration` (with the priority order in rows 167-168); (d) populate `authorize_url`, `token_url`, `code_challenge_methods_supported` from the discovered metadata when `OAuthConfig.discovery=true`. Existing `discovery_allowlist` SSRF guard (`config.rs:104-110`) becomes load-bearing instead of decorative. Largest single MCP-spec gap right now.
  - **Eval**: hybrid (rmcp 1.7.0 ships `auth` feature with `AuthorizationManager` in `rmcp::transport::auth` which already implements RFC 8414/OIDC + PRM discovery — but `runtime.rs` doesn't use it; it threads bearer tokens directly via `auth_header()`; switching to `AuthorizationManager` or adopting its discovery sub-modules is the right path) · non-trivial (openab-agent refactor ~300-500 LOC: replace pre-resolved `authorize_url`/`token_url` with discovery-on-401, plumb through `discovery_allowlist` SSRF guard) · **fit: in-scope** (this is exactly what rmcp's auth feature is for; we're reinventing it badly). Largest single payoff for spec MUST compliance.
- [x] **Rows 183-190, 211, 242 (RFC 8707 `resource` parameter)**: DONE for custom providers. `flow.rs::canonical_resource` canonicalises `ServerConfig::Http.url` per RFC 8707 §2; `flow.rs::init_paste_authorize` takes `resource: Option<&str>` and appends it to the authorize URL; `runtime.rs` token POSTs (`post_token_exchange`/`post_token_refresh`/`post_device_token_poll`/`post_device_authorization`) append `("resource", …)` to the form; `resolve_paste_client`/`resolve_device_client` compute the gated resource and `PendingPasteLogin.resource` snapshots it for `complete_login`. **Gating**: built-in Anthropic skips `resource` (its AS ≠ the MCP server URL; no evidence it honors `resource`; sending it risks `invalid_target` breaking the shipping login) — interim divergence from the unconditional "regardless of AS support" MUST (row 190), revisit once PRM/discovery (rows 153-168) lets the client learn the true audience. NB: real MCP OAuth flow is `flow.rs`+`runtime.rs`, not `auth.rs` (legacy Codex) — the old plan's `auth.rs:357/410/493/585` callsites were wrong.
  - **Eval**: openab-agent only (rmcp `AuthorizationManager` may surface a `resource` setter once we adopt it; until then we own the param injection) · drop-in (~60 LOC: canonical URI helper + 5 callsite injections) · **fit: in-scope**. Pure URL-builder work, MUST-level spec compliance, no architectural commitments. Highest ROI security item.
- [ ] **Rows 161-162, 164, 207, 207a (`WWW-Authenticate` parsing + step-up reauth)**: when rmcp surfaces `StreamableHttpError::AuthRequired` / `InsufficientScope` (already carrying `required_scope`), trigger reauth flow with the challenge-provided scope set rather than bubbling up. Hook in `meta_tool.rs` tool-call path. Couples with row 183-190 (resource param needs to be re-sent on step-up).
  - **Eval**: rmcp already does the heavy lifting (parses `WWW-Authenticate` and surfaces structured `AuthRequired(AuthRequiredError { www_authenticate_header })` / `InsufficientScope { required_scope }` errors per SDK `transport/common/reqwest/streamable_http_client.rs:136-166`) · openab-agent drop-in (~100 LOC: catch the two error variants in `meta_tool.rs`, route to existing OAuth flow with new scope set, retry once) · **fit: in-scope**. We're already half-built — rmcp surfaces what we need, just not consumed yet. Pairs naturally with PRM discovery.
- [ ] **Rows 220, 222, 223 (PKCE methods verification)**: once AS-metadata discovery lands, check `code_challenge_methods_supported` contains `S256` before issuing the request; abort with clear error if absent. Until discovery is done, document the "always send PKCE, trust the AS" behaviour as a known soft-violation.
  - **Eval**: openab-agent only, blocked on PRM/AS-metadata discovery work (rows 153-168) · drop-in once unblocked (~15 LOC: existence check + clear error) · **fit: in-scope**. Trivial given discovery; meaningless until then. Document as known gap meanwhile.
- [x] **Rows 217, 218 (HTTPS / localhost enforcement for custom providers)**: `OAuthConfig::validate` (`config.rs:131`) now rejects non-`https://` `authorize_url` / `token_url` and non-loopback/non-`https` `redirect_uri` for custom providers (`oauth::builtin(p).is_none()` gate + `is_loopback_or_https_redirect` helper). 5 tests added.
  - **Eval**: openab-agent only · drop-in (~15 LOC scheme check + tests) · **fit: in-scope**. MUST-level spec compliance, near-zero implementation cost, no rmcp coupling. Ship this first as cheap quick-win.
- [ ] **Row 213 (secure token storage)**: optionally back `auth.json` with an OS keyring (`keyring` crate) when available; fall back to filesystem mode. Low priority unless we hear of a leak vector; current model is adequate for single-user dev hosts.
  - **Eval**: openab-agent only (rmcp leaves token persistence to consumer) · non-trivial (~150 LOC: feature-gated `keyring` crate, fallback path, cross-platform testing on Linux/macOS) · **fit: defer — low value for our deploy targets**. SHOULD spec; openab-agent's main deploy targets are containers (no OS keyring) so fallback is the common case anyway. Revisit if a leak vector forces it.
- [ ] **Documentation**: `openab-agent/docs/` should call out (a) the PRM / RFC 8707 gap with explicit "what works without spec compliance" matrix, (b) supported / unsupported registration mechanisms (pre-registered only, no CIMD, no DCR), (c) which built-in providers exist and which env vars wire their client IDs.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Sets reader expectation honestly; matches our docs-first culture. Cheap.

## Cancellation

Source: [`basic/utilities/cancellation.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/cancellation.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 243 | `notifications/cancelled` carries `requestId` and optional `reason` | (notification) | ✅ (schema) | rmcp `CancelledNotificationParam { request_id, reason: Option<String> }` (SDK `model.rs:696-699`) — both fields present per schema |
| 244 | Cancellation notification MUST only reference requests issued in the same direction | MUST | N/A (vacuously) | we send no `notifications/cancelled` today (no `notify_cancelled` callsite in `src/mcp/**`); if/when we do via rmcp `peer.notify_cancelled` (SDK `service/client.rs:368`) the direction will be client→server, matching same-direction requirement |
| 245 | Cancellation notification MUST only target requests believed in-progress | MUST | N/A (vacuously) | no cancel emission today. Future: rmcp `RequestHandle::cancel` (SDK `service.rs:349-360`) drops the handle's receiver after sending — naturally only references in-progress request id |
| 246 | `initialize` request MUST NOT be cancelled by clients | MUST NOT | ✅ | initialize is performed inside `().serve(transport).await` at `runtime.rs:1066,1079`; rmcp owns the request id and never exposes it externally, so we cannot cancel it even if we wanted to |
| 247 | For task-augmented requests, the `tasks/cancel` request MUST be used instead of the `notifications/cancelled` notification (tasks have a dedicated cancellation that returns final state) | MUST | N/A | we do not use task-augmented requests (no `tasks` client capability per Section 1 row 55); `tasks/cancel` is the task transport's surface, not ours |
| 248 | Receivers SHOULD stop processing, free resources, not respond | SHOULD | N/A | we never receive `notifications/cancelled` from server — client handler is bare `()` (no `on_cancelled` impl); rmcp default discards via the `ClientHandler` blanket impl (SDK `handler/client.rs:46`) |
| 249 | Receivers MAY ignore cancellation if request unknown / complete / uncancellable | MAY | N/A | as above; we are effectively the "ignore" path by virtue of having no handler |
| 250 | Sender SHOULD ignore any late response to a cancelled request | SHOULD | ✅ | the timeout path (rows 69-71) drives `await_response()`, which consumes `rx` before emitting the cancellation — any late response is discarded by the transport worker (SDK `service.rs:323-326`) |
| 251 | Both parties MUST handle cancel race conditions gracefully | MUST | ✅ | now active via rows 69-71: rmcp's auto-cancel-on-timeout (SDK `service.rs:332-343`) is race-safe (`rx` consumed before notification sent); inherited by both `meta_tool.rs` request sites |
| 252 | Both parties SHOULD log cancellation reasons | SHOULD | ✅ | both `meta_tool.rs` request sites log `tracing::info!(target:"mcp.cancel", server, tool?, timeout_secs, "… sent notifications/cancelled")` on `ServiceError::Timeout` — the openab-layer reason log atop rmcp's internal emission |
| 253 | Application UIs SHOULD indicate cancellation state | SHOULD | N/A | openab-agent is a CLI/meta-tool gateway — no UI surface. ACP `session/cancel` (`acp.rs:91-92`) is the closest surface but only a transport hook, not a UI |
| 254 | Invalid cancellation notifications SHOULD be ignored | SHOULD | ✅ | rmcp `ClientHandler` default `on_cancelled` discards unknown notifications (no panic / error propagation); we inherit this via `()` handler — bare `()` impl satisfies the SHOULD by routing unknown notifications to no-op |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Major finding (worth highlighting)**: rmcp 1.7.0 already implements the full cancel-on-timeout pattern internally — `RequestHandle::await_response` (SDK `service.rs:322-343`) auto-emits `CancelledNotification` with `reason="request timeout"` when `PeerRequestOptions.timeout` expires. We just don't use the option today (our `peer.call_tool(params).await` path doesn't go through the option-bearing API). This collapses Section 1 rows 69-71 + row 70 + Section 4 row 252 into a single switch: route `call_tool` via `peer.send_request_with_option(req, PeerRequestOptions::new().with_timeout(d))` then `await_response().await`.

- [x] **Row 252 (Log cancellation reasons)** — DONE: `tracing::info!(target:"mcp.cancel", server, …, timeout_secs, …)` logged at both `meta_tool.rs` request sites on `ServiceError::Timeout`, atop rmcp's internal emission.
  - **Eval**: openab-agent only (rmcp emits the notification on its own but doesn't expose a hook for application-level logging without rebuilding the path) · drop-in (~10 LOC if we own the `send_request_with_option` wrapper) · **fit: in-scope**. Trivial extension of the timeout work; matches our existing tracing-only observability rule.
- [x] **(consolidate with Section 1 rows 69-71 + row 70)** — DONE: switched `call_tool` (`tools/call`) and `fetch_tools` (`tools/list`) in `meta_tool.rs` from `peer.call_tool(params).await` / `peer.list_all_tools().await` to the option-bearing path. `tools/list` is now a manual `next_cursor` pagination loop (rmcp's `list_all_tools` takes no options) — each page bounded by the same timeout. Gives (a) per-request timeout, (b) auto `CancelledNotification` (`reason="request timeout"`), (c) race-safe response discard in one change. Construction note: rmcp request structs are `#[non_exhaustive]` — built via `CallToolRequest::new(params)` / `ListToolsRequest::with_param(PaginatedRequestParams::default().with_cursor(cursor))` and `PeerRequestOptions::no_options()` + field assignment (no `with_timeout` builder exists).
  - **Eval**: hybrid (rmcp covers the heavy lifting — `PeerRequestOptions`, `RequestHandle::await_response`, auto-cancel emission; openab-agent owns the callsite switch) · drop-in (~50 LOC) · **fit: in-scope — high value**. Previous Section 1 eval for Row 70 (called it "non-trivial, ~120 LOC") was WRONG — rmcp ships this pattern out of the box. Correcting that eval as part of this improvement.

## Progress

Source: [`basic/utilities/progress.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/progress.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 255 | `progressToken` carried in request `_meta` | (field) | ❌ | we never populate `_meta.progressToken` on outbound `peer.call_tool(params)` — `CallToolRequestParams::new(...).with_arguments(...)` at `meta_tool.rs:95-98` only sets `name` + `arguments`. So no server can stream progress back to us today |
| 256 | Progress tokens MUST be string or integer | MUST | ✅ (schema) | `rmcp::model::ProgressToken(pub NumberOrString)` (SDK `model.rs:305`) — `NumberOrString` enforces string-or-integer at the type level |
| 257 | Progress tokens MUST be unique across active requests | MUST | N/A (vacuously) | we send no `progressToken` (row 255); if implemented, would need a per-`McpRuntimeManager` token counter or UUID source |
| 258 | `notifications/progress` carries token, progress, optional total/message | (notification) | ✅ (schema) | `rmcp::model::ProgressNotificationParam { progress_token, progress: f64, total: Option<f64>, message: Option<String> }` (SDK `model.rs:1100-1115`); method const `notifications/progress` |
| 259 | `progress` value MUST increase with each notification, even if total is unknown | MUST | N/A | we send no progress; on receive we discard via `()` handler. The SDK doc-comment on `progress: f64` matches the spec wording (SDK `model.rs:1107-1108`) but rmcp does not enforce monotonicity on either send or receive — it's the application's burden |
| 260 | `progress` and `total` MAY be float | MAY | ✅ (schema) | both fields are `f64` in rmcp schema (SDK `model.rs:1108, 1111`) |
| 261 | `message` field SHOULD provide relevant human-readable progress information | SHOULD | N/A | we send no progress; rmcp provides `with_message(impl Into<String>)` builder (SDK `model.rs:1135`) |
| 262 | Progress notifications MUST only reference active in-progress operation tokens | MUST | N/A (vacuously) | we send no progress; if implemented, drop the token when request completes / cancels |
| 263 | Receivers MAY skip notifications / set frequency / omit total | MAY | ✅ | `()` client handler (`runtime.rs:1066,1079`) means rmcp routes incoming `ProgressNotification` to the blanket `ClientHandler::on_progress` default-impl which discards (SDK `handler/client.rs:201-203, 321-326`); we "skip" by virtue of having no handler |
| 264 | For task-augmented requests, the `progressToken` from the original request MUST continue to be used for progress notifications throughout task lifetime — valid until the task reaches a terminal status, even after `CreateTaskResult` returns | MUST | N/A | we do not implement task augmentation (Section 1 row 55 ❌); spec item only applies if we adopt `tasks` capability |
| 265 | Progress notifications for tasks MUST use the original `progressToken` | MUST | N/A | as above |
| 266 | Progress notifications for tasks MUST stop after terminal status | MUST | N/A | as above |
| 267 | Senders and receivers SHOULD track active progress tokens | SHOULD | N/A (vacuously) | we send no progress (row 255), we discard incoming progress (row 263) — nothing to track |
| 268 | Both parties SHOULD implement rate limiting on progress notifications | SHOULD | N/A (vacuously) | as above; rmcp does no rate limiting either way |
| 269 | Progress notifications MUST stop after completion | MUST | N/A (vacuously) | as above; spec compliance is the sender's responsibility |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 255 (emit `progressToken` on outbound requests)**: optionally set `_meta.progressToken` on `CallToolRequestParams` at `meta_tool.rs:95-98` so servers running long operations (file scans, large API queries, build steps) can stream progress back to us. Token would be a monotonically increasing per-server counter or a UUID; `()` client handler would be upgraded to a wrapper that routes `on_progress` to ACP `session/notify` so the orchestrator/user sees live updates.
  - **Eval**: hybrid (rmcp covers most of the plumbing — `ProgressToken`, `ProgressNotificationParam`, `ClientHandler::on_progress` trait method all exist; `CallToolRequestParams` already supports `_meta` via `with_meta` builder or direct field; openab-agent owns the wrapper + bridge) · non-trivial (~150-200 LOC: token allocator, `on_progress` → ACP notify bridge, token-to-request-id mapping for row 262 compliance) · **fit: borderline — architectural threshold**. Requires us to leave `()` handler — same architectural threshold as Section 1 rows 52-56 (client capabilities). Worth bundling with `roots` capability work. Until then, document as known gap.
- [ ] **Row 268 (rate limiting on receive side)**: if we adopt outbound `progressToken` (row 255), add a simple `tokio::sync::Semaphore` or "max 10 notifications/sec per token" filter in the `on_progress` handler to protect the ACP/UI surface from chatty servers.
  - **Eval**: openab-agent only · drop-in (~30 LOC throttled stream) · **fit: defer**. Only meaningful once row 255 lands; revisit when row 255 ships.
- [ ] **Documentation**: in the same `openab-agent/docs/` matrix as Section 3 / Section 0 docs improvements, note that we do not currently emit or surface progress, and what users should expect for long-running tools.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Cheap; honest about UX gap.

## Ping

Source: [`basic/utilities/ping.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/ping.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 270 | `ping` request method | (method) | ✅ (schema) | rmcp `PingRequest` model (SDK `model/meta.rs:141, 164`) + dispatched at `handler/client.rs:19` |
| 271 | Receiver MUST respond promptly with empty `{}` result | MUST | ✅ | `()` client handler (`runtime.rs:1066,1079`) inherits `ClientHandler::ping` default-impl returning `Ok(())` mapped to `ClientResult::empty` (SDK `handler/client.rs:85-90, 19`); rmcp ignores (does not reply to) pre-handshake pings, logging them via a trace path (SDK `service/client.rs:130-135`) |
| 272 | If no response within timeout, sender MAY consider connection stale / terminate / reconnect | MAY | N/A (vacuously) | we send no outbound pings (no `peer.send_request_with_option(PingRequest, ...)` callsite); rmcp does not expose a convenience `peer.ping()` method — sending requires manual `PingRequest` construction |
| 273 | Implementations SHOULD periodically issue pings to detect connection health | SHOULD | ❌ | no periodic ping loop in `runtime.rs` / `breaker.rs`; we rely on next tool-call failure to detect stale connection (which trips the breaker via `record_tool_call_outcome` at `meta_tool.rs:100, 104`) |
| 274 | Ping frequency SHOULD be configurable | SHOULD | ❌ | no ping config field on `ServerConfig::Stdio` / `ServerConfig::Http` (`config.rs:21-37`); `McpConfig` itself at `config.rs:13-16` is just a server-list wrapper |
| 275 | Ping timeouts SHOULD be appropriate for network environment | SHOULD | N/A | no pinging |
| 276 | Excessive pinging SHOULD be avoided | SHOULD | ✅ (vacuously) | we send zero pings — vacuously below any reasonable rate limit |
| 277 | Ping timeouts SHOULD be treated as connection failures | SHOULD | N/A | no pinging |
| 278 | Multiple failed pings MAY trigger connection reset | MAY | N/A | no pinging |
| 279 | Implementations SHOULD log ping failures for diagnostics | SHOULD | N/A | no pinging to log |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Rows 273-279 (Periodic outbound ping for connection-health)**: add a per-server periodic ping loop. Spawn a `tokio::task` per `ServerHandle::client` that fires `PingRequest` every `ping_interval` (default 30s, configurable); on timeout (default 5s) emit `tracing::warn!(target="mcp.ping", server=%name, reason=%e)` and increment `breaker.rs` failure counter so repeated failures trip the breaker. Wired via the timeout-bearing `send_request_with_option` path (unified with Section 1 rows 69-71 + Section 4 finding).
  - **Eval**: hybrid (rmcp upstream supplies `PingRequest` (SDK `model/meta.rs:141`) + `send_request_with_option`; openab-agent layer adds the wrapper) · drop-in (~80 LOC: ping task spawn at `connect()` / cleanup at `disconnect()`, config fields on `ServerConfig`, tracing + breaker hook) · **fit: in-scope**, drop-in once Section 1 timeout work lands. Spec is SHOULD across the board; main value is proactive detection of half-open HTTP connections that don't surface as transport errors until next call.
- [ ] **Row 274 specifically (configurable frequency)**: surface `ping_interval_secs` + `ping_timeout_secs` fields on `ServerConfig::Stdio` / `ServerConfig::Http` (`config.rs:21-37`); when both are `None`, default to no pinging (opt-in). Avoids ping cost for short-lived servers (typical stdio meta-tool flow).
  - **Eval**: openab-agent only · drop-in (~10 LOC config additions + threading through to ping task) · **fit: in-scope**. Same shape as rows 69-71 timeout config — keep schema consistent.

## Tasks — experimental

Source: [`basic/utilities/tasks.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/tasks.mdx)

**Section-level finding**: openab-agent does NOT implement the experimental `tasks` capability. We declare no `capabilities.tasks` in `clientInfo` (Section 1 row 55 ❌, `()` handler at `runtime.rs:1066,1079`). rmcp 1.7.0 ships Task schema types (`Task`, `CreateTaskResult`, `TaskStatus` etc. in `model/task.rs`) and server-side handler defaults (`handler/server.rs:336-578`), but no client-side peer convenience methods for `tasks/get` / `tasks/list` / `tasks/cancel` / `tasks/result`. Most rows below are therefore N/A or vacuously satisfied — we don't declare the capability, we don't send task-augmented requests, and we don't receive task-related notifications. Server-side rows (rows 292-360 majority) are off-surface for us; row 282 is the client-capability schema (also N/A by abstention).

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 280 | Servers and clients supporting tasks MUST declare `tasks` capability during init | MUST | N/A (vacuously) | we don't support tasks; not declaring is the conforming path for non-supporters |
| 281 | Server `tasks` capability includes `list`, `cancel`, `requests.tools.call` | (capability) | N/A | server-side schema |
| 282 | Client `tasks` capability includes `list`, `cancel`, `requests.sampling.createMessage`, `requests.elicitation.create` | (capability) | N/A | we don't declare (no `sampling` / `elicitation` either — Section 1 rows 53-54 ❌) |
| 282a | `capabilities.tasks.requests` is exhaustive — request types not listed do NOT support task-augmentation | (capability) | N/A | semantic note for the capability schema |
| 283 | Requestors SHOULD only augment requests with task if receiver declared the capability | SHOULD | ✅ (vacuously) | we never augment requests with task metadata; `meta_tool.rs:95-98` `CallToolRequestParams::new(...).with_arguments(...)` doesn't touch `_meta.task` |
| 284 | If `capabilities.tasks` not defined, peer SHOULD NOT attempt to create tasks | SHOULD NOT | ✅ (vacuously) | as a client we don't create tasks. The "peer" side (server) compliance is its concern; we trigger the precondition (no `capabilities.tasks` declared) which protects us |
| 285 | Tool-level: if server lacks `tasks.requests.tools.call`, clients MUST NOT attempt task augmentation regardless of `taskSupport` | MUST NOT | ✅ (vacuously) | we never attempt task augmentation on any tool |
| 286 | Tool-level: if `execution.taskSupport` absent or `"forbidden"`, clients MUST NOT invoke as task | MUST NOT | ✅ (vacuously) | as above |
| 287 | Tool-level: server SHOULD return `-32601` if client attempts task-augmentation on forbidden tool | SHOULD | N/A | server-side |
| 288 | Tool-level: if `execution.taskSupport == "optional"`, clients MAY invoke as task or normal | MAY | ❌ (we always go normal) | no task-mode invocation; we always call tools normally. Acceptable per "MAY" |
| 289 | Tool-level: if `execution.taskSupport == "required"`, clients MUST invoke as task | MUST | ❌ | **gap**: if a server marks a tool `taskSupport: "required"`, we cannot invoke it (we don't implement task augmentation). Such tools would 405/`-32601` on normal `tools/call` and we'd surface as transport error |
| 290 | Tool-level: server MUST return `-32601` if `taskSupport == "required"` and client does not use task | MUST | N/A | server-side; client-side: we'd see this as a transport/protocol error today, no special handling |
| 291 | Requestors MAY include `ttl` value (ms) in task augmentation | MAY | N/A | we don't task-augment |
| 292 | Receivers accepting task-augmented request MUST return `CreateTaskResult` | MUST | N/A | server-side |
| 293 | `CreateTaskResult` SHOULD be returned as soon as possible after accepting | SHOULD | N/A | server-side |
| 293a | `CreateTaskResult` `_meta` MAY include `io.modelcontextprotocol/model-immediate-response` (string) suggesting an immediate-return value for the LLM while the task executes (provisional, non-binding) | MAY | N/A | server-side |
| 294 | Requestors SHOULD respect `pollInterval` in responses for polling frequency | SHOULD | N/A | we don't poll |
| 295 | Requestors SHOULD continue polling until terminal status or `input_required` | SHOULD | N/A | as above |
| 296 | Even after invoking `tasks/result`, requestors SHOULD continue polling via `tasks/get` unless actively blocked waiting on the `tasks/result` response | SHOULD | N/A | as above |
| 297 | Receivers MAY send `notifications/tasks/status` on status change | MAY | N/A | server-side |
| 298 | `notifications/tasks/status` includes full `Task` object | (notification) | N/A | schema |
| 299 | Requestors MUST NOT rely on receiving `notifications/tasks/status` (it is optional) | MUST NOT | ✅ (vacuously) | we receive nothing task-related (no handler) |
| 300 | When sent, `notifications/tasks/status` SHOULD NOT include `io.modelcontextprotocol/related-task` metadata | SHOULD NOT | N/A | server-side |
| 301 | `tasks/list` operation supports pagination | (method) | N/A | server-side method |
| 302 | Receivers MUST reject `tasks/cancel` on already-terminal tasks with `-32602` | MUST | N/A | server-side |
| 303 | Upon valid cancellation, receivers SHOULD attempt to stop execution and MUST transition to `cancelled` before responding | SHOULD/MUST | N/A | server-side |
| 304 | Once cancelled, task MUST remain in `cancelled` even if execution completes | MUST | N/A | server-side |
| 305 | Receivers MAY delete cancelled tasks at any time | MAY | N/A | server-side |
| 306 | Requestors SHOULD NOT rely on cancelled tasks being retained | SHOULD NOT | ✅ (vacuously) | we cancel nothing |
| 307 | Receivers without task capability for a request type MUST process normally, ignoring task metadata | MUST | N/A | server-side |
| 308 | Receivers with task capability for a request type MAY return an error for non-task-augmented requests, effectively requiring task augmentation | MAY | N/A | server-side; from the client perspective, duplicates row 289 gap |
| 309 | Task IDs MUST be string values | MUST | N/A | server-side |
| 310 | Task IDs MUST be generated by the receiver | MUST | N/A | server-side |
| 311 | Task IDs MUST be unique among all tasks controlled by the receiver | MUST | N/A | server-side |
| 312 | Tasks MUST begin in `working` status | MUST | N/A | server-side |
| 313 | Receivers MUST only transition through valid paths: from `working` → `input_required`/`completed`/`failed`/`cancelled`; from `input_required` → `working`/`completed`/`failed`/`cancelled` | MUST | N/A | server-side |
| 314 | Terminal tasks (`completed`/`failed`/`cancelled`) MUST NOT transition to any other status | MUST NOT | N/A | server-side |
| 314a | For task-augmented `tools/call`, if the underlying tool result has `isError: true`, the task should reach `failed` status | SHOULD | N/A | server-side |
| 315 | When task needs requestor input, receiver SHOULD move task to `input_required` | SHOULD | N/A | server-side |
| 316 | When in `input_required`, receiver MUST include `io.modelcontextprotocol/related-task` metadata in any request it sends back to the requestor (e.g., the elicitation/sampling that the task depends on) | MUST | N/A | server-side |
| 317 | When requestor encounters `input_required`, it SHOULD preemptively call `tasks/result` | SHOULD | N/A | we don't encounter (no polling) |
| 318 | When receiver receives required input, task SHOULD transition out of `input_required` | SHOULD | N/A | server-side |
| 319 | Receivers MUST include `createdAt` ISO 8601 timestamp in all task responses | MUST | N/A | server-side |
| 320 | Receivers MUST include `lastUpdatedAt` ISO 8601 timestamp in all task responses | MUST | N/A | server-side |
| 321 | Receivers MAY override requested `ttl` | MAY | N/A | server-side |
| 322 | Receivers MUST include actual `ttl` (or `null` for unlimited) in `tasks/get` responses | MUST | N/A | server-side |
| 323 | After `ttl` elapsed, receivers MAY delete task and results | MAY | N/A | server-side |
| 324 | Receivers MAY include `pollInterval` (ms) in `tasks/get` responses | MAY | N/A | server-side |
| 325 | On `tasks/result` for terminal task, receiver MUST return the underlying request's final result/error | MUST | N/A | server-side |
| 326 | On `tasks/result` for non-terminal task, receiver MUST block the response until task reaches terminal status | MUST | N/A | server-side |
| 327 | For terminal tasks, `tasks/result` MUST return exactly what the original request would | MUST | N/A | server-side |
| 328 | All requests/notifications/responses related to a task MUST include `io.modelcontextprotocol/related-task` metadata | MUST | N/A | requestor + receiver; we don't task |
| 329 | For `tasks/get`/`tasks/result`/`tasks/cancel`, `taskId` param MUST be source of truth | MUST | N/A | we don't issue these |
| 330 | Requestors SHOULD NOT include `io.modelcontextprotocol/related-task` metadata in `tasks/get`/`tasks/result`/`tasks/cancel` request params (the `taskId` RPC param is source of truth) | SHOULD NOT | N/A | as above |
| 330a | Receivers SHOULD NOT include related-task metadata in result messages for `tasks/get`/`tasks/list`/`tasks/cancel` (taskId already in response) | SHOULD NOT | N/A | server-side |
| 331 | Receivers MUST ignore related-task metadata if present in `tasks/get`/`tasks/result`/`tasks/cancel` requests, treating `taskId` RPC param as source of truth | MUST | N/A | server-side |
| 332 | `tasks/result` response MUST include the related-task metadata | MUST | N/A | server-side |
| 333 | Receivers SHOULD use cursor-based pagination for `tasks/list` | SHOULD | N/A | server-side |
| 334 | Receivers MUST include `nextCursor` if more tasks available | MUST | N/A | server-side |
| 335 | Requestors MUST treat cursors as opaque tokens | MUST | ✅ (vacuously) | we don't request `tasks/list` |
| 336 | If task retrievable via `tasks/get`, it MUST be retrievable via `tasks/list` for same requestor | MUST | N/A | server-side |
| 337 | If `tasks/result` underlying request resulted in JSON-RPC error, `tasks/result` MUST return same error | MUST | N/A | server-side |
| 338 | If `tasks/result` underlying request returned response, `tasks/result` MUST return that response | MUST | N/A | server-side |
| 339 | Receivers MUST return `-32602` for invalid/nonexistent `taskId` in get/result/cancel | MUST | N/A | server-side |
| 340 | Receivers MUST return `-32602` for invalid/nonexistent cursor in `tasks/list` | MUST | N/A | server-side |
| 341 | Receivers MUST return `-32602` for cancellation of terminal task | MUST | N/A | server-side |
| 342 | Receivers MUST return `-32603` for internal errors | MUST | N/A | server-side |
| 343 | Receivers MAY return `-32600` if task augmentation required but not provided | MAY | N/A | server-side |
| 344 | Receivers SHOULD provide informative error messages | SHOULD | N/A | server-side |
| 345 | `tasks/get` response on failure SHOULD include diagnostic `statusMessage` | SHOULD | N/A | server-side |
| 346 | When auth context available, receivers MUST bind tasks to that context | MUST | N/A | server-side |
| 347 | If context-binding unavailable, receivers SHOULD document the limitation | SHOULD | N/A | server-side |
| 348 | If context-binding unavailable, receivers MUST generate cryptographically secure task IDs | MUST | N/A | server-side |
| 349 | Receivers unable to identify requestors SHOULD NOT declare `tasks.list` capability | SHOULD NOT | N/A | server-side |
| 350 | With context binding, receivers MUST reject cross-context `tasks/get`/`tasks/result`/`tasks/cancel` | MUST | N/A | server-side |
| 351 | With context binding, `tasks/list` results MUST include only tasks for requestor's context | MUST | N/A | server-side |
| 352 | Receivers SHOULD implement rate limiting on task operations | SHOULD | N/A | server-side |
| 353 | Receivers SHOULD enforce concurrent-task limits per requestor | SHOULD | N/A | server-side |
| 354 | Receivers SHOULD enforce maximum `ttl` to prevent indefinite retention | SHOULD | N/A | server-side |
| 355 | Receivers SHOULD clean up expired tasks promptly | SHOULD | N/A | server-side |
| 356 | Receivers SHOULD document max supported `ttl` and max concurrent tasks per requestor | SHOULD | N/A | server-side |
| 357 | Receivers SHOULD implement monitoring/alerting for resource usage | SHOULD | N/A | server-side |
| 358 | Receivers SHOULD log task creation/completion/retrieval events for audit | SHOULD | N/A | server-side |
| 359 | Receivers SHOULD include auth context in logs when available | SHOULD | N/A | server-side |
| 360 | Receivers SHOULD monitor for suspicious patterns | SHOULD | N/A | server-side |
| 361 | Requestors SHOULD log task lifecycle events for debugging/audit | SHOULD | N/A (vacuously) | no task lifecycle on client side |
| 362 | Requestors SHOULD track task IDs and associated operations | SHOULD | N/A (vacuously) | no task IDs |
| 362a | On Streamable HTTP, clients MAY disconnect from an SSE stream opened in response to `tasks/get` or `tasks/result` at any time | MAY | N/A | we don't issue `tasks/*` requests |
| 362b | Servers SHOULD NOT upgrade to an SSE stream in response to a `tasks/get` request | SHOULD NOT | N/A | server-side |
| 362c | Clients SHOULD expect task-related messages to be delivered on any SSE stream (including the HTTP GET stream) | SHOULD | N/A | we expect nothing task-related; if rmcp surfaces an unexpected task message via SSE, the `()` handler discards it |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 289 (servers with `taskSupport: "required"` tools)**: surface a clear `ServerStatus`-level diagnostic when a tool's declared `taskSupport == "required"` AND we cannot invoke it. Today such a tool surfaces as a generic protocol error; add detection in `meta_tool.rs::fetch_tools` (lines 116-132, just before `Ok(tools)`) that inspects each tool's `execution.taskSupport` field and marks the tool as `unavailable` with a user-facing reason like "requires task augmentation (not implemented)".
  - **Eval**: openab-agent only (rmcp `Tool` schema includes `execution` field) · drop-in (~30 LOC tool-filter pass) · **fit: in-scope as defensive UX**. Bounds the unsupported-tool failure mode without committing us to implement `tasks`.
- [ ] **(Whole `tasks` capability — DEFER)**: implementing client-side `tasks` is a substantial architectural commitment — task polling loop, `tasks/get` issuance, `notifications/tasks/status` handler (which requires upgrading from `()` to a custom `ClientHandler` impl), task-ID tracking, ACP `session/notify` integration for long-running task progress. Bundle with Section 1 client-capability work (`roots` first, `sampling`/`elicitation`/`tasks` later) only when a real server use case demands it.
  - **Eval**: openab-agent layer (rmcp has schema types but no convenience APIs for `tasks/*`) · architectural commitment (~600-1000 LOC: handler upgrade + polling task + state model) · **fit: borderline — defer until demand**. Spec section is explicitly "experimental"; ecosystem servers using `taskSupport: "required"` are still rare. Revisit when first real server breaks.
- [ ] **Documentation**: clearly state we do not support `tasks` capability so server authors know to fall back to non-task-augmented `tools/call`. Goes in the same docs matrix as Section 0 / 3 / 5 gaps.
  - **Eval**: docs only · drop-in · **fit: in-scope**.

## Client / Roots

Source: [`client/roots.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/roots.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 363 | Clients supporting roots MUST declare `roots` capability | MUST | N/A | we don't support roots. Named `OpenabClientHandler` (`runtime.rs`, the `()`-replacement keystone) does **not** override `get_info()`, so it inherits the trait default `ClientInfo::default()` → `ClientCapabilities::default()` with no `roots` field (SDK `handler/client.rs:257-259`, `model/capabilities.rs:260`) — identical advertised capabilities to the prior `()` handler. Vacuously satisfied: by not declaring, the MUST does not bind us. |
| 364 | `roots.listChanged` sub-capability | (capability) | N/A | not declared (see #363) |
| 365 | `roots/list` request method | (method) | ✅ | `OpenabClientHandler::list_roots` (`runtime.rs`) overrides the SDK default and returns `Err(ErrorData::method_not_found::<ListRootsRequestMethod>())` (`-32601`) — a server that calls `roots/list` despite our missing capability now gets the correct method-not-found error rather than an empty list. See #370. |
| 366 | `roots/list` result: `roots[]` of `{uri, name}` | (field) | ✅ (schema) | `rmcp::model::ListRootsResult { roots: Vec<Root> }` + `Root { uri, name }` (SDK `model.rs:2462-2466` for `Root`, `:2487-2493` for `ListRootsResult`) |
| 367 | Root `uri` MUST be `file://` URI | MUST | N/A | we never construct a `Root`. Spec MUST applies only when emitting roots |
| 368 | Root `name` is optional | (field) | N/A | we never construct a `Root` |
| 369 | On roots change, `listChanged`-capable clients MUST send `notifications/roots/list_changed` | MUST | N/A | we don't declare `listChanged` (see #364). SDK exposes `peer.notify_roots_list_changed` (SDK `service/client.rs:371`) if we ever need it |
| 370 | Clients SHOULD return `-32601` (method not found) if roots unsupported, `-32603` for internal | SHOULD | ✅ | honored precisely: `OpenabClientHandler::list_roots` returns `Err(ErrorData::method_not_found::<ListRootsRequestMethod>())` = `-32601` (`runtime.rs`), replacing the SDK default's empty list |
| 371 | Clients MUST only expose roots with appropriate permissions | MUST | N/A | we expose no roots |
| 372 | Clients MUST validate all root URIs (path traversal) | MUST | N/A | we expose no roots |
| 373 | Clients MUST implement proper access controls | MUST | N/A | we expose no roots |
| 374 | Clients MUST monitor root accessibility | MUST | N/A | we expose no roots |
| 375 | Servers SHOULD handle unavailable roots gracefully | SHOULD | N/A | server-side requirement; we are the client |
| 376 | Servers SHOULD respect root boundaries during operations | SHOULD | N/A | server-side |
| 377 | Servers SHOULD validate all paths against provided roots | SHOULD | N/A | server-side |
| 378 | Clients SHOULD prompt user consent before exposing roots | SHOULD | N/A | we expose no roots. Would apply if we implement #363 |
| 379 | Clients SHOULD provide clear UI for root management | SHOULD | N/A | we expose no roots; also no interactive UI surface in openab-agent (ACP relays to host) |
| 380 | Clients SHOULD validate root accessibility before exposing | SHOULD | N/A | we expose no roots |
| 381 | Clients SHOULD monitor for root changes | SHOULD | N/A | we expose no roots |
| 382 | Servers SHOULD check for roots capability before usage | SHOULD | N/A | server-side |
| 383 | Servers SHOULD handle root list changes gracefully | SHOULD | N/A | server-side |
| 384 | Servers SHOULD cache root information appropriately | SHOULD | N/A | server-side |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Document we don't declare `roots` capability**: extend the same Section 0 / 3 / 5 / 7 client-capability docs matrix to call out that openab-agent advertises no `roots` capability, so spec-compliant servers will skip `roots/list` entirely. Note that any server that ignores capability and calls `roots/list` anyway now receives a correct `-32601` method-not-found (via `OpenabClientHandler::list_roots`, row 657), not an empty list.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Same matrix expansion pattern used in earlier sections.
- [x] **Override SDK default `list_roots` to return `-32601`**: DONE. Added `pub struct OpenabClientHandler` (`src/mcp/runtime.rs`) impl `rmcp::ClientHandler`, overriding only `list_roots` → `Err(ErrorData::method_not_found::<ListRootsRequestMethod>())`; swapped the blanket `()` at the three `RunningService<RoleClient, …>` type params + two `.serve()` callsites. `get_info()` deliberately not overridden so advertised capabilities stay byte-identical (rows 363/365). This named struct is the **keystone**: the 🟡 group (`on_tool_list_changed` row 503, `on_resource_updated` §12, `on_prompt_list_changed` §13, elicitation-complete §10) now extends this same struct rather than needing the `()` swap first.
  - **Eval**: openab-agent only (rmcp `ClientHandler` trait stable) · drop-in · **fit: in-scope** — landed as the keystone that unblocks the wiring-only group.
- [ ] **(Whole `roots` capability — DEFER)**: implementing client-side `roots` means committing to a workspace/path model (which directories does an openab-agent session expose? the ACP `cwd`? a configured allow-list?), declaring `capabilities.roots.listChanged`, plumbing `notify_roots_list_changed` on workspace changes, and enforcing #371–#374 (permission/validation/access-control/accessibility monitoring). The natural source of truth is the ACP session's working directory plus any host-supplied allow-list, but we don't currently surface either to the MCP layer.
  - **Eval**: openab-agent layer (rmcp has `Root`/`ListRootsResult`/`notify_roots_list_changed` primitives, no convenience layer) · architectural commitment (~400-700 LOC: handler upgrade + workspace model + path validator + change-watcher + ACP cwd wiring) · **fit: borderline — defer until demand**. No filesystem-scoped MCP server currently in our deployment matrix needs it; revisit when a server explicitly requires `roots` (parallel trigger to Section 7 `tasks`).
- [x] **Document the `()` → SDK default behavior as a known divergence**: SUPERSEDED — the divergence no longer exists. With `list_roots` now returning `-32601` (row 657 implemented), rows 365 + 370 are genuinely ✅ rather than "SDK default empty list", so there is nothing to document as a divergence.
  - **Eval**: docs only · resolved by the row 657 keystone landing.

## Client / Sampling

Source: [`client/sampling.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/sampling.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 385 | Clients supporting sampling MUST declare `sampling` capability | MUST | N/A | we don't support sampling. `()` client handler at `runtime.rs:1066,1079` → `ClientHandler for ()` (SDK `handler/client.rs:263`) → `get_info()` returns `ClientInfo::default()` (SDK `handler/client.rs:257`) → `ClientCapabilities::default()` (SDK `model.rs:914`) with no `sampling` field. Grep across `src/` for `sampling`/`CreateMessage`/`create_message` returns zero hits — vacuously compliant by abstention |
| 386 | Clients supporting tool-enabled sampling MUST declare `sampling.tools` capability | MUST | N/A | same as 385 — we don't support sampling, so `sampling.tools` is moot |
| 387 | Servers MUST NOT send tool-enabled sampling to clients without `sampling.tools` capability | MUST NOT | N/A | server-side normative |
| 387a | Client MUST return an error if `CreateMessageRequestParams.tools` is provided but client did not declare `ClientCapabilities.sampling.tools` (symmetric to row 387, per `schema.mdx` JSDoc) | MUST | ⚠️ (vacuous) | default `ClientHandler::create_message` (SDK `handler/client.rs:92-100`) unconditionally returns `McpError::method_not_found::<CreateMessageRequestMethod>()` regardless of whether `tools` is present. Servers should never call us in the first place (no `sampling` declared), but if one does, we reject the whole request — not specifically the `tools` field. Acceptable since we don't claim sampling support |
| 388 | `sampling.context` sub-capability (soft-deprecated) — servers SHOULD NOT use `includeContext` values `thisServer`/`allServers` unless client declares it | SHOULD NOT | N/A | server-side |
| 389 | Servers SHOULD avoid `includeContext` `thisServer`/`allServers` (soft-deprecated) | SHOULD | N/A | server-side |
| 390 | `sampling/createMessage` request | (method) | ❌ | not implemented. Default `ClientHandler::create_message` (SDK `handler/client.rs:92-100`) returns `method_not_found`. We're an LLM client ourselves (`src/llm.rs` exposes `AnthropicProvider` / `OpenAiProvider` via `LlmProvider` trait, used by `src/agent.rs:6` and `src/acp.rs:2,133-146`), so we *could* implement sampling by routing back to our own provider — but we don't today, and we don't advertise the capability |
| 391 | Request params: `messages`, `modelPreferences`, `systemPrompt`, `maxTokens`, `includeContext` (default `"none"`) | (field) | N/A | SDK `CreateMessageRequestParams` schema is provided by rmcp 1.7.0; we don't consume any of these fields |
| 391a | Client MAY ignore `modelPreferences` (per `schema.mdx` JSDoc) | MAY | N/A | we don't process sampling requests |
| 391b | Client MAY modify or omit `systemPrompt` (per `schema.mdx` JSDoc) | MAY | N/A | same |
| 391c | Client MAY ignore `includeContext` (per `schema.mdx` JSDoc) | MAY | N/A | same |
| 391d | Client MAY sample fewer tokens than `maxTokens` requested (per `schema.mdx` JSDoc) | MAY | N/A | same |
| 392 | Request params (tools): optional `tools[]`, `toolChoice` | (field) | N/A | SDK schema only; not consumed |
| 393 | Result fields: `role`, `content`, `model`, `stopReason` | (field) | N/A | we never produce `CreateMessageResult` |
| 394 | Content types: text / image / audio / tool_use / tool_result | (field) | N/A | not consumed |
| 394a | Client SHOULD preserve `ToolUseContent._meta` for caching optimizations (per `schema.mdx` JSDoc) | SHOULD | N/A | we don't process sampling content |
| 394b | Client SHOULD preserve `ToolResultContent._meta` for caching optimizations (per `schema.mdx` JSDoc) | SHOULD | N/A | same |
| 395 | Tool-result user messages MUST contain ONLY tool results (no mixing) | MUST | N/A | sampling response shape; we don't construct sampling messages |
| 396 | Every assistant `ToolUseContent` block MUST be followed by user message of `ToolResultContent` matching by `toolUseId` | MUST | N/A | same |
| 397 | `toolChoice` modes: `auto`, `required`, `none` | (field) | N/A | not consumed |
| 398 | `toolChoice: required` — model MUST use at least one tool before completing | MUST | N/A | not applicable — we don't run sampling |
| 399 | `toolChoice: none` — model MUST NOT use any tools | MUST NOT | N/A | same |
| 400 | Model preferences: `costPriority`, `speedPriority`, `intelligencePriority` (0–1) | (field) | N/A | not consumed |
| 401 | Model `hints[].name` substring-match | (field) | N/A | not consumed |
| 401a | Client MUST evaluate `ModelPreferences.hints` in array order (per `schema.mdx` JSDoc) | MUST | N/A | not consumed |
| 401b | Client SHOULD prioritize `hints` over numeric priorities; MAY use numeric priorities as fallback (per `schema.mdx` JSDoc) | SHOULD | N/A | not consumed |
| 402 | Clients MAY map hints to equivalent models from different providers | MAY | N/A | not consumed |
| 402a | Client MAY ignore `ModelHint.meta` (non-standard model-specific metadata, per `schema.mdx` JSDoc) | MAY | N/A | not consumed |
| 403 | Human-in-the-loop SHOULD be able to deny sampling requests | SHOULD | N/A | no sampling = no human-in-the-loop surface needed; would become applicable if 390 is implemented |
| 404 | Applications SHOULD provide UI to review requests, edit prompts, present responses | SHOULD | N/A | gated on implementing 390 |
| 405 | Clients SHOULD return errors for common failures (`-1` user rejected, `-32602` tool-result missing, `-32602` tool-results mixed) | SHOULD | ⚠️ | default handler returns `method_not_found` (`-32601`) for every sampling call. Spec-compliant for "we don't support this", but not the granular error catalog the row describes (only matters once we implement sampling) |
| 406 | Clients SHOULD implement user approval controls | SHOULD | N/A | gated on implementing 390 |
| 407 | Both parties SHOULD validate message content | SHOULD | N/A | we don't process sampling messages; rmcp deserializes via `CreateMessageRequestParams` |
| 408 | Clients SHOULD respect model preference hints | SHOULD | N/A | gated on implementing 390 |
| 409 | Clients SHOULD implement rate limiting | SHOULD | N/A | gated on implementing 390 |
| 410 | Both parties MUST handle sensitive data appropriately | MUST | N/A | vacuously satisfied — we never read/forward sampling payloads |
| 411 | When replying to a `stopReason: "toolUse"` response, servers MUST respond to each `ToolUseContent` with a `ToolResultContent` of matching `toolUseId` | MUST | N/A | server-side (spec reads "servers MUST respond") |
| 412 | When tools are used, user message containing tool results MUST contain only tool results | MUST | N/A | sampling-response shape; not constructed by us |
| 413 | Both parties SHOULD implement iteration limits for tool loops | SHOULD | N/A | gated on implementing 390 |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Document the abstention explicitly** in this section and in a short `docs/mcp-client-capabilities.md` (or inline `src/mcp/mod.rs` comment) noting we use `()` as `ClientHandler` and therefore do not advertise `sampling` / `sampling.tools` / `roots` / `elicitation`.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Cheap, future-reader-friendly, prevents the next person from wondering whether the absence is intentional.
- [ ] **Add an integration test** that spins up a mock rmcp server, sends a `sampling/createMessage` request to our client, and asserts we respond with `method_not_found` (`-32601`). Pins the "vacuously compliant by abstention" claim against accidental future opt-in.
  - **Eval**: test layer · drop-in (~50 LOC if rmcp test infra exists) · **fit: borderline**. Guards 387a/390/405 simultaneously; only useful once we have any other rmcp-handler tests to amortize fixture cost — defer if test harness isn't there yet.
- [ ] **Implement `ClientHandler::create_message`** by routing to our existing `LlmProvider` (`src/llm.rs` — `AnthropicProvider`, `OpenAiProvider`) and declare `capabilities.sampling = {}` (no `tools` sub-cap initially). Forms a "sampling pass-through": MCP server delegates an LLM call to us, we use the user's already-authenticated provider.
  - **Eval**: openab-agent layer · architectural commitment (~300-500 LOC: custom `ClientHandler` struct replacing `()`, `CreateMessageRequestParams` → `llm::Message` mapping, content-block conversion, model-hint resolution, and an approval UX through the ACP front-end) · **fit: defer**. Sampling exists so server can borrow client's LLM; we'd be routing back to our own Anthropic/OpenAI client — concretely useful only when a server we actually run requests it. No demand signal today.
- [ ] **If 390 is implemented, add `sampling.tools` sub-capability support**: pass `tools[]` / `toolChoice` through to the `LlmProvider`, enforce `toolChoice: required`/`none` semantics, and surface `ToolUseContent` / `ToolResultContent` ordering rules (rows 395, 396, 411, 412).
  - **Eval**: openab-agent layer · non-trivial (~200-300 LOC: tool-schema bridge from MCP → `llm::ToolDef`, response post-validation, `stopReason: "toolUse"` mapping; additive on top of previous item) · **fit: defer**. Strictly dependent on the previous item landing; revisit only if a real MCP server we use starts requesting tool-enabled sampling.
- [ ] **If 390 is implemented, add human-in-the-loop approval surface** (rows 403/404/406): route `CreateMessageRequestParams` through an ACP `request_permission` prompt before invoking the `LlmProvider`, with an option to edit `systemPrompt` / `messages` before sending.
  - **Eval**: openab-agent + acp layer · non-trivial (~150-250 LOC: new ACP permission kind, prompt rendering, edit round-trip) · **fit: defer**. Required by spec (SHOULD) the moment we advertise `sampling`; treat as a hard prerequisite, not a follow-up, if 390 is ever picked up.
- [ ] **If 390 is implemented, add per-server sampling rate limit (row 409) and tool-loop iteration cap (row 413)** — both configurable via `config.toml` per MCP server entry, defaults conservative (e.g. 10 calls/min, 8 tool iterations).
  - **Eval**: openab-agent layer · drop-in (~50-100 LOC: token-bucket per server id, counter on tool-loop response chain) · **fit: defer**. Cheap insurance against runaway server costs; depends on 390 landing first, so bundle with it.

## Client / Elicitation

Source: [`client/elicitation.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/elicitation.mdx)

**Section-level finding**: openab-agent uses the unit `()` `ClientHandler` blanket impl (`src/mcp/runtime.rs:1066,1079`; `RoleClient, ()` peer type at `src/mcp/runtime.rs:67,228,1056`). The default `ClientHandler::create_elicitation` returns `ElicitationAction::Decline` (SDK `handler/client.rs:165-178`), and we never set `ClientCapabilities.elicitation` (SDK `model/capabilities.rs:276`), so every server-initiated `elicitation/create` is silently declined and no `form` / `url` mode is advertised. ACP (`src/acp.rs`) currently only routes `session/prompt` (`src/acp.rs:87-89`) and has no `session/request_permission` surface to pass an elicitation through to Brett. Most spec rows are therefore N/A-by-omission; they re-activate only if we ship row 414 (declare a capability) — see Improvement Plan below.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 414 | Clients supporting elicitation MUST declare `elicitation` capability | MUST | N/A | we do not support elicitation. No `ClientCapabilities.elicitation` set anywhere in `src/mcp/`; default `()` handler declines. Gating row for everything below — see Improvement Plan §1 |
| 415 | Empty `elicitation: {}` is equivalent to declaring `form` mode only | (compat) | N/A | not declared. rmcp models the two sub-caps explicitly (`FormElicitationCapability` / `UrlElicitationCapability` at SDK `model/capabilities.rs:203,215`); when we declare we should opt into one sub-cap (not an empty object) to avoid the legacy implicit |
| 416 | Clients declaring `elicitation` MUST support at least one mode (`form` or `url`) | MUST | N/A | not declared |
| 417 | Servers MUST NOT send elicitation requests with modes not supported by the client | MUST NOT | N/A | server-side rule; we are the client |
| 418 | All elicitation requests MUST include `message`; `mode` is required for URL mode and optional (defaults to `"form"`) for form mode | MUST | N/A | server-side payload rule. rmcp wire model enforces via `CreateElicitationRequestParams::{FormElicitationParams, UrlElicitationParams}` (SDK `model.rs:2637`) — internally `#[serde(tag = "mode", try_from = "…DeserializeHelper")]` with an untagged backwards-compat variant for absent `mode` (helper at `model.rs:2536-2562`) |
| 419 | For backwards compat, servers MAY omit `mode` for form mode requests | MAY | N/A | server-side. rmcp's deserialise-helper routes the absent-`mode` shape to `FormElicitationParams` (SDK `model.rs:2536-2562`); regression test at `model.rs:3888` |
| 420 | Clients MUST treat absent `mode` as form mode | MUST | ✅ | inherited from rmcp's `CreateElicitationRequestParams` deserialise-helper (SDK `model.rs:2536-2562,2637`); whatever handler we plug in receives a `FormElicitationParams` variant — same code path as explicit `mode:"form"` |
| 421 | Form mode elicitation MUST either specify `mode:"form"` or omit `mode`, and include `requestedSchema` | MUST | N/A | server-side payload shape; enforced by rmcp model |
| 422 | `requestedSchema` is restricted: flat object, primitive types (string, number/integer, boolean, enum) | (constraint) | ✅ (SDK) | rmcp's `PrimitiveSchema` enum (SDK `model/elicitation_schema.rs:53-`) restricts to String/Number/Integer/Boolean/Enum; non-primitive payloads fail deserialisation before reaching our (currently-default) handler |
| 422a | `requestedSchema` also supports multi-select enum: `type: "array"` with `items.enum` or `items.anyOf` (fifth schema kind beyond the primitives in row 422) | (constraint) | ✅ (SDK) | `MultiSelectEnumSchema` at SDK `model/elicitation_schema.rs:730-732` (Untitled + Titled variants, `type:"array"`, `items.enum`) |
| 423 | Supported string formats: `email`, `uri`, `date`, `date-time` | (constraint) | ✅ (SDK) | `StringSchema` format field in SDK `model/elicitation_schema.rs` (covered by builder helpers like `required_email`) |
| 424 | All primitive types support optional default values | (field) | ✅ (SDK) | defaults present on primitive schemas in SDK `model/elicitation_schema.rs` |
| 425 | Clients supporting defaults SHOULD pre-populate form fields with default values | SHOULD | N/A | no form UI to pre-populate; gated on row 414 |
| 426 | URL mode elicitation MUST specify `mode:"url"`, `message`, `url`, `elicitationId` | MUST | N/A | server-side. rmcp `UrlElicitationParams` variant struct enforces (SDK `model.rs:2654`) |
| 427 | `url` parameter MUST contain a valid URL | MUST | N/A | server-side; we'd revalidate before opening (gated on 414) |
| 428 | Servers MAY send `notifications/elicitation/complete` on URL-mode completion | MAY | N/A | server-side |
| 429 | Servers MUST only send completion notification to the client that initiated the elicitation | MUST | N/A | server-side |
| 430 | Completion notification MUST include the original `elicitationId` | MUST | ✅ (SDK) | `ElicitationResponseNotificationParam.elicitation_id` (SDK `model.rs:2746`); notification method const at SDK `model.rs:2513` |
| 430a | Client MUST treat `ElicitRequestURLParams.elicitationId` as opaque (per `schema.mdx` JSDoc) | MUST | ✅ | typed `String` in SDK `model.rs:2667,2746`; our (default) handler at SDK `handler/client.rs:241-247` never parses it — just no-ops |
| 431 | Clients MUST ignore completion notifications for unknown / already-completed IDs | MUST | ⚠️ | default handler `on_url_elicitation_notification_complete` (SDK `handler/client.rs:241-247`) silently drops every notification — technically satisfies "ignore unknown" since all are unknown to us, but only because we never accept URL elicitation in the first place. Becomes a real obligation when row 414 lands |
| 432 | Clients MAY wait for completion notification to retry / update UI / continue | MAY | N/A | we don't accept URL elicitation |
| 433 | Clients SHOULD still provide manual retry/cancel controls if notification never arrives | SHOULD | N/A | same as 432 |
| 434 | Servers MAY return `URLElicitationRequiredError` (-32042) | MAY | N/A | server-side rule. rmcp 1.7.0 ships the code: `ErrorCode::URL_ELICITATION_REQUIRED = -32042` (SDK `model.rs:509`) + helper `ErrorData::url_elicitation_required()` (SDK `model.rs:562-567`) |
| 435 | Server MUST NOT return `URLElicitationRequiredError` except when URL elicitation required | MUST NOT | N/A | server-side |
| 436 | The error MUST include list of required elicitations | MUST | ⚠️ | rmcp 1.7.0 exposes the error code but the structured `data: { elicitations: [...] }` payload required by the 2025-11-25 spec is not modelled — callers hand-roll JSON in `ErrorData.data: Option<Value>` (SDK `model.rs:529`). Track as schema gap; file upstream |
| 437 | Elicitations in error MUST be URL mode and have `elicitationId` | MUST | ⚠️ | same gap as 436 — no typed wrapper in rmcp 1.7.0 |
| 438 | Servers MUST return `-32042` when request blocked on URL elicitation | MUST | N/A | server-side |
| 439 | Clients MUST return `-32602` when elicitation mode not declared in capabilities | MUST | ❌ | default `()` handler returns `Ok(Decline)` (SDK `handler/client.rs:171-178`) instead of `Err(-32602)`. Spec-compliant behaviour when no `elicitation` cap is declared would be to reject as `INVALID_PARAMS`; we silently decline. Low-risk because servers ought to gate on row 414 first, but technically non-conformant. Fix in Improvement Plan §2 |
| 440 | Servers MUST NOT request sensitive info via form mode (passwords, API keys, access tokens, payment credentials) | MUST NOT | N/A | server-side |
| 441 | Servers MUST use URL mode for sensitive info interactions | MUST | N/A | server-side |
| 442 | Clients MUST provide UI making it clear which server is requesting | MUST | N/A | no UI surface yet; gated on row 414 + ACP `session/request_permission` (`src/acp.rs:87-89` only handles `session/prompt`) |
| 443 | Clients MUST respect privacy with clear decline/cancel options | MUST | N/A | same as 442 |
| 444 | For form mode, clients MUST allow user review/modify before sending | MUST | N/A | same as 442 |
| 445 | For URL mode, clients MUST clearly display target domain/host and gather user consent before navigation | MUST | N/A | same as 442 |
| 446 | Three-action response model: accept / decline / cancel | (field) | ✅ (SDK) | `ElicitationAction::{Accept, Decline, Cancel}` in rmcp 1.7.0 (SDK `model.rs:2515+`); default `()` impl returns `Decline` |
| 447 | Servers MUST bind elicitation requests to client and user identity | MUST | N/A | server-side |
| 448 | Servers implementing elicitation MUST securely associate user state per security best practices | MUST | N/A | server-side |
| 449 | State MUST NOT be associated with session IDs alone | MUST NOT | N/A | server-side |
| 450 | State storage MUST be protected against unauthorized access | MUST | N/A | server-side |
| 451 | Remote servers MUST derive user identification from MCP authorization credentials when possible | MUST | N/A | server-side |
| 452 | MCP servers MUST NOT rely on URL elicitation to authorize users for themselves | MUST NOT | N/A | server-side |
| 453 | Third-party credentials MUST NOT transit through the MCP client | MUST NOT | ✅ | we never proxy URL-elicitation payloads; default decline + opaque-only handling at SDK `handler/client.rs:165-178,241-247`. Vacuously satisfied today; needs re-audit if we implement URL mode (Improvement Plan §3) |
| 454 | MCP server MUST NOT use client's MCP credentials for third-party service (no token passthrough) | MUST NOT | N/A | server-side. Our paste-back / device flows (`src/mcp/runtime.rs:255-345,347-550`) keep server credentials separate |
| 455 | User MUST authorize MCP server directly for external authorization | MUST | N/A | server-side |
| 456 | MCP server MUST NOT transmit credentials obtained via URL elicitation to MCP client | MUST NOT | N/A | server-side |
| 457 | Servers MUST NOT include sensitive info / PII / credentials in elicitation URL | MUST NOT | N/A | server-side |
| 458 | Servers MUST NOT provide pre-authenticated URLs (impersonation risk) | MUST NOT | N/A | server-side |
| 459 | Servers SHOULD NOT include clickable URLs in form-mode fields | SHOULD NOT | N/A | server-side |
| 460 | Servers SHOULD use HTTPS URLs for non-development environments | SHOULD | N/A | server-side |
| 461 | Clients implementing URL mode MUST handle URLs carefully (prevent malicious links) | MUST | N/A | we don't implement URL mode |
| 462 | Clients MUST NOT auto-prefetch elicitation URLs or metadata | MUST NOT | ✅ | default handler at SDK `handler/client.rs:165-178` never touches the URL — no HTTP client, no fetch |
| 463 | Clients MUST NOT open URL without explicit user consent | MUST NOT | ✅ | same — we never open it |
| 464 | Clients MUST show full URL for user examination before consent | MUST | N/A | no consent UI; gated on row 414 |
| 465 | Clients MUST open URL in secure manner (no LLM/client inspection of content) | MUST | N/A | same as 464 |
| 466 | Clients SHOULD highlight URL domain to mitigate subdomain spoofing | SHOULD | N/A | same as 464 |
| 467 | Clients SHOULD warn on ambiguous/suspicious URIs (Punycode) | SHOULD | N/A | same as 464 |
| 468 | Clients SHOULD NOT render URLs as clickable in elicitation fields except the URL-mode `url` field | SHOULD NOT | N/A | same as 464 |
| 469 | Servers MUST NOT rely on client-provided user ID without server verification | MUST NOT | N/A | server-side |
| 470 | Servers SHOULD follow security best practices for user identification | SHOULD | N/A | server-side |
| 471 | Clients SHOULD validate all form responses against provided schema | SHOULD | N/A | no response generated. Note: rmcp gates schema validation behind opt-in `FormElicitationCapability { schema_validation: Some(true) }` (SDK `model/capabilities.rs:203-210` + builder `enable_elicitation_schema_validation` at `:559`). When we land row 414 we should opt in |
| 472 | Servers SHOULD validate received data matches requested schema | SHOULD | N/A | server-side |
| 473 | Servers MUST verify identity of user opening URL before accepting info (anti-phishing) | MUST | N/A | server-side |
| 474 | Server MUST ensure user who started elicitation is same user who completes authorization flow | MUST | N/A | server-side |
| 475 | Mechanism to determine user identity MUST be resilient to attacks where an attacker can modify the elicitation URL | MUST | N/A | server-side |
| 476 | Clients SHOULD implement user approval controls | SHOULD | N/A | gated on row 414 + ACP surface |
| 477 | Clients SHOULD allow users to decline elicitation requests at any time | SHOULD | ⚠️ | today we auto-decline 100% (default handler) — technically over-satisfies "user can decline" but degenerate. Real obligation lands with row 414 |
| 478 | Clients SHOULD implement rate limiting | SHOULD | N/A | no accept path → no rate-limit surface yet |
| 479 | Clients SHOULD present elicitation requests clearly (what / why) | SHOULD | N/A | gated on row 414 |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **§1. Decide on elicitation as a client capability.** Either declare `ClientCapabilities { elicitation: Some(ElicitationCapability { form: Some(FormElicitationCapability { schema_validation: Some(true) }), url: Some(UrlElicitationCapability {}) }), .. }` (the two sub-cap structs land in `CreateElicitationRequestParams::{FormElicitationParams, UrlElicitationParams}` on the wire) and implement a real `ClientHandler` (replacing the `()` blanket at `src/mcp/runtime.rs:1066,1079` with a named struct that holds an `Arc<AcpClient>` channel), or stay opt-out and document the intentional gap so the ⚠️ rows (431/439/477) downgrade to ✅-by-design.
  - **Eval**: openab-agent layer · non-trivial (~250-400 LOC: custom `ClientHandler` struct + `ClientCapabilities` plumbing in `runtime.rs::Dial::run` + ACP `session/request_permission` surface in `src/acp.rs` adjacent to existing `session/prompt` dispatch at `:87-89` + form-schema rendering for Brett) · **fit: in-scope**. Elicitation is the cleanest map onto our existing ACP human-in-the-loop UX — likely the first MCP client capability worth wiring end-to-end, and the prerequisite for unblocking every other N/A row in this section.
- [ ] **§2. Make the not-supported path return `-32602` explicitly (row 439).** Until §1 ships, override `create_elicitation` with an impl that returns `Err(ErrorData::invalid_params("elicitation capability not declared", None))` instead of inheriting the default-decline (SDK `handler/client.rs:171-178`).
  - **Eval**: openab-agent only · drop-in (~20 LOC: one `ClientHandler` impl on a zero-sized struct, swap `()` at `src/mcp/runtime.rs:1066,1079`) · **fit: in-scope**. Cheap precondition for §1 and removes the one concrete ❌ row in the section. Worth doing standalone even if §1 slips.
- [ ] **§3. Re-audit rows 453 / 462 / 463 once §1 lands.** They are currently ✅ vacuously (we don't accept URL mode); when we wire a real handler we must keep credential isolation and the no-prefetch invariant in the new code path.
  - **Eval**: openab-agent layer · drop-in (test + doc only) · **fit: in-scope**. Cheap follow-up tied to §1's PR.
- [ ] **§4. File rmcp upstream issue for `URLElicitationRequiredError` payload schema (rows 436/437).** rmcp 1.7.0 ships the `-32042` code constant + `ErrorData::url_elicitation_required()` helper (SDK `model.rs:509,562-567`) but no typed `{ elicitations: [...] }` payload struct; servers and clients have to hand-roll JSON in `ErrorData.data: Option<Value>`.
  - **Eval**: rmcp upstream · non-trivial (schema PR + serde derives + cross-version compat for 2025-06-18 vs 2025-11-25) · **fit: borderline — file an issue, don't fork**. We are a client; even when §1 ships we only *receive* this error from servers we connect to (and can hand-parse `data` defensively until upstream lands a typed wrapper). Worth raising so the broader rmcp community converges.
- [ ] **§5. After §1, add `notifications/elicitation/complete` handling.** Override `on_url_elicitation_notification_complete` (SDK `handler/client.rs:241-247`) to look up the `elicitation_id` in a per-server pending map (similar in spirit to `device_login_tasks` at `src/mcp/runtime.rs:129`) and surface the result back through ACP. Implement row 431 (ignore unknown IDs) as a real `HashMap::get` miss, not "we ignore everything".
  - **Eval**: openab-agent layer · non-trivial (~100-150 LOC: pending-elicitation map with TTL + ACP fan-out wired into the new `ClientHandler` from §1) · **fit: in-scope (blocked on §1)**. Natural follow-up once URL mode is real; until then it's premature work.

## Server / Tools

Source: [`server/tools.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx)

**Section-level finding**: openab-agent is the **client**; the tools surface is the only MCP capability we actively consume. `src/mcp/meta_tool.rs` wires `fetch_tools` via `peer.list_all_tools` (auto-paginated, `src/mcp/meta_tool.rs:116-132`; SDK `service/client.rs:378-392`) and `call_tool` via `peer.call_tool` with `CallToolRequestParams::new(name).with_arguments(args)` (`src/mcp/meta_tool.rs:95-98`). The result projection in `list_tools` / `describe_tool` (`src/mcp/meta_tool.rs:139-161`) is intentionally minimal: only `name` + `description` are surfaced to the LLM, and `describe_tool` adds `input_schema`. `title`, `annotations`, `icons`, `execution`, `output_schema` are all dropped before the LLM sees them. The `()` `ClientHandler` blanket impl (`src/mcp/runtime.rs:1066,1079`) means `on_tool_list_changed` no-ops silently. The agent layer (`src/agent.rs:184`) always emits the outer meta-tool `ToolResult` with `is_error: None` regardless of inner `CallToolResult.is_error`. Server-side rows 480/481/485-491/497/499/500/502/504a/505/511-514 are N/A by topology.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 480 | Servers with tools MUST declare `tools` capability | MUST | N/A | server-side normative; we are an MCP client (`src/mcp/runtime.rs:1066,1079` use `()` ClientHandler) |
| 481 | `tools.listChanged` sub-capability bit | (capability) | N/A | server-declared capability; client just reads it via rmcp `ServerCapabilities`; no openab-agent surface |
| 482 | `tools/list` with pagination | (method) | ✅ | auto-paginated by rmcp `Peer::list_all_tools` (SDK `service/client.rs:378-392`); called at `src/mcp/meta_tool.rs:122` |
| 483 | `tools/call` with `name` + `arguments` | (method) | ✅ | `src/mcp/meta_tool.rs:95-98` builds `CallToolRequestParams::new(name).with_arguments(args_map)`, dispatched via `peer.call_tool` |
| 484 | Tool fields: name / title / description / inputSchema / outputSchema / annotations / icons / execution | (field) | ⚠️ | rmcp `Tool` carries all fields; our `list_tools` projection collapses to `{name, description}` only (`src/mcp/meta_tool.rs:139-143`). `describe_tool` adds `input_schema` (`:157-161`). `title`/`annotations`/`icons`/`execution`/`output_schema` silently dropped |
| 485 | `inputSchema` MUST be a valid JSON Schema object (not null) | MUST | N/A | server obligation; client forwards `t.input_schema` to LLM opaquely (`src/mcp/meta_tool.rs:160`) |
| 486 | `inputSchema` follows JSON Schema usage guidelines (default 2020-12) | (constraint) | N/A | server-side schema authoring constraint |
| 486a | For tools with no parameters, `inputSchema` SHOULD be `{"type":"object","additionalProperties":false}` (recommended) or `{"type":"object"}` | SHOULD | N/A | server-authoring recommendation |
| 487 | `outputSchema` follows JSON Schema usage guidelines (default 2020-12) | (constraint) | N/A | server-side schema authoring constraint |
| 488 | Tool names SHOULD be 1–128 characters (inclusive) | SHOULD | N/A | server-naming SHOULD; client does not validate name length |
| 488a | Tool names SHOULD be considered case-sensitive | SHOULD | ✅ | case-sensitive `String` equality in tool lookup (`src/mcp/meta_tool.rs:155` `find(|t| t.name.as_ref() == tool)`) |
| 489 | Tool names SHOULD only contain A-Z, a-z, 0-9, `_`, `-`, `.` | SHOULD | N/A | server-naming SHOULD; client accepts any name string |
| 490 | Tool names SHOULD NOT contain spaces / commas / special chars | SHOULD NOT | N/A | server-naming SHOULD NOT; client accepts any name string |
| 491 | Tool names SHOULD be unique within a server | SHOULD | N/A | server uniqueness obligation; we trust server |
| 492 | `execution.taskSupport` values: `"forbidden"` (default), `"optional"`, `"required"` | (field) | ❌ | `execution` field never read or surfaced; `describe_tool` (`src/mcp/meta_tool.rs:157-161`) returns name/description/input_schema only. We will silently invoke a tool whose server declared `taskSupport:"required"` even though we do not support tasks (see Section 7) |
| 493 | Tool result content types: text / image / audio / resource_link / resource (embedded) | (field) | ⚠️ | `CallToolResult` round-tripped via `serde_json::to_value` (`src/mcp/meta_tool.rs:109`); LLM sees raw JSON, no per-type rendering or fan-out |
| 494 | Content types support optional annotations (audience / priority / lastModified) | (field) | ⚠️ | annotations survive JSON round-trip but ignored — no `audience`/`priority` filtering before LLM sees content (`src/mcp/meta_tool.rs:109`) |
| 495 | Tool MAY return `resource_link` items | MAY | ⚠️ | forwarded verbatim in serialized result; no fetch / dereference (`src/mcp/meta_tool.rs:109`) |
| 496 | Tool result MAY embed `resource` items | MAY | ⚠️ | embedded resources forwarded verbatim, never specially rendered |
| 497 | Servers using embedded resources SHOULD implement `resources` capability | SHOULD | N/A | server-side obligation |
| 498 | Result: `content[]`, `isError`, optional `structuredContent` | (field) | ⚠️ | all three round-trip via rmcp `CallToolResult` (SDK `model.rs:2774-2787`); meta_tool emits whole struct to LLM but `src/agent.rs:184` does not branch on inner `is_error` |
| 499 | Tools returning structured content SHOULD also return serialized JSON in a `TextContent` block (for backwards compatibility) | SHOULD | N/A | server-side SHOULD; client consumes whichever side is present |
| 500 | If `outputSchema` provided, servers MUST provide structured results matching | MUST | N/A | server obligation |
| 501 | If `outputSchema` provided, clients SHOULD validate structured results against it | SHOULD | ❌ | no JSON-Schema validator wired; `call_tool` serializes result without checking `structured_content` vs `output_schema` (`src/mcp/meta_tool.rs:98-109`) |
| 502 | List-changed-capable servers SHOULD send `notifications/tools/list_changed` | SHOULD | N/A | server-side emit obligation |
| 503 | `notifications/tools/list_changed` notification | (notification) | ❌ | `()` ClientHandler uses default `on_tool_list_changed` no-op (SDK `handler/client.rs`); no cache refresh, no LLM notification |
| 504 | Two error mechanisms: protocol errors (JSON-RPC) + tool execution errors (`isError: true`) | (model) | ✅ | wire `Err` trips breaker + bubbles `anyhow` (`src/mcp/meta_tool.rs:103-107`); wire `Ok` with `isError:true` resets breaker (comment at `:96-97`, runtime hook) |
| 504a | Errors originating from tool execution SHOULD be reported inside `CallToolResult` (with `isError: true`), not as JSON-RPC protocol errors (per `schema.mdx` JSDoc) | SHOULD | N/A | server-emit SHOULD; client respects whichever shape arrives |
| 505 | Input validation errors are classified as tool execution errors (`isError: true`), not protocol errors | (classification) | N/A | server-classification obligation |
| 506 | Clients SHOULD provide tool execution errors to LLMs for self-correction | SHOULD | ⚠️ | full `CallToolResult` (incl. `is_error`) serialized to LLM (`src/mcp/meta_tool.rs:109`), but outer meta-tool ToolResult always emits `is_error: None` (`src/agent.rs:184`); LLM has to parse inner JSON |
| 507 | Clients MAY provide protocol errors to LLMs | MAY | ✅ | `anyhow::Error::new(e).with_context(...)` (`src/mcp/meta_tool.rs:105-106`) bubbles back through dispatch and renders as tool error to LLM |
| 508 | Clients MUST consider tool annotations untrusted unless from trusted server | MUST | ⚠️ | annotations stripped from `list_tools` projection (`src/mcp/meta_tool.rs:139-143`); LLM never sees them, so de-facto untrusted by omission. No explicit trust model documented |
| 509 | Human-in-the-loop SHOULD be able to deny tool invocations | SHOULD | ❌ | no interactive deny path in dispatch (`src/mcp/meta_tool.rs`); meta-tool calls fire on LLM decision without HITL gate |
| 510 | Apps SHOULD show exposed tools + visual indicators + confirmation prompts | SHOULD | ❌ | no UI surface; openab-agent is headless ACP/CLI — no tool catalog UI or confirmation prompt |
| 511 | Servers MUST validate all tool inputs | MUST | N/A | server obligation |
| 512 | Servers MUST implement proper access controls | MUST | N/A | server obligation |
| 513 | Servers MUST rate-limit tool invocations | MUST | N/A | server obligation (client-side circuit breaker in `src/mcp/breaker.rs` is transport-failure protection, not rate-limit) |
| 514 | Servers MUST sanitize tool outputs | MUST | N/A | server obligation |
| 515 | Clients SHOULD prompt for confirmation on sensitive operations | SHOULD | ❌ | no confirmation prompt path; dispatch goes straight to `peer.call_tool` (`src/mcp/meta_tool.rs:98`) |
| 516 | Clients SHOULD show tool inputs to user before calling server | SHOULD | ❌ | headless; no pre-call display of `arguments` to user. ACP frame surfaces afterward, not before |
| 517 | Clients SHOULD validate tool results before passing to LLM | SHOULD | ❌ | `call_tool` only `serde_json::to_value` then returns (`src/mcp/meta_tool.rs:109`); no schema check / sanitization |
| 518 | Clients SHOULD implement timeouts for tool calls | SHOULD | ✅ | implemented via `PeerRequestOptions { timeout }` around both `tools/call` and `tools/list` in `meta_tool.rs` (`request_timeout_secs` per-server, default 60s); on expiry breaker-fed error + rmcp auto-cancel |
| 519 | Clients SHOULD log tool usage for audit | SHOULD | ❌ | `meta_tool.rs` has no `tracing::info!` for call/list invocations; only `record_tool_call_outcome` updates breaker state (`:100,104`) |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 484 — enrich `list_tools` / `describe_tool` projection** to surface `title`, `annotations`, `icons`, `execution`, `output_schema` so the LLM can route on `taskSupport` and respect `audience`/`priority`. Today `list_tools` (`src/mcp/meta_tool.rs:139-143`) collapses to `{name, description}` only.
  - **Eval**: openab-agent only · drop-in (~30 LOC, projection-only) · **fit: in-scope**. Pure additive JSON shape; no rmcp change required.
- [ ] **Row 492 — honour `execution.taskSupport`** by refusing `call` actions on tools that declare `"required"` (we don't implement tasks) and surfacing the value to LLM via `describe_tool`.
  - **Eval**: openab-agent only · drop-in (~20 LOC guard + describe_tool field) · **fit: in-scope**. Bounds the unsupported-tool failure mode that Section 7 Improvement #1 also flags; pair the patches.
- [ ] **Row 501 — validate structured results against `outputSchema`** when both are present; emit warning + downgrade to text on mismatch.
  - **Eval**: openab-agent layer · non-trivial (~80 LOC + `jsonschema` crate dependency) · **fit: borderline**. New dep is the cost; behaviour is SHOULD not MUST. Defer until first concrete divergence observed.
- [ ] **Row 503 — wire `on_tool_list_changed`** to invalidate the per-server tools cache (planned in `src/mcp/meta_tool.rs:113` comment) and re-emit tool list to LLM. Same blocker as Section 10 §1 elicitation — needs replacing `()` blanket impl with a named `ClientHandler` struct.
  - **Eval**: openab-agent layer · non-trivial (~60 LOC bundled with Section 10 §1 ClientHandler refactor) · **fit: in-scope (bundled)**. Free once the named-handler struct lands.
- [ ] **Row 506 — propagate inner `CallToolResult.is_error` to outer meta-tool ToolResult** at `src/agent.rs:184` (currently always `is_error: None`). Lets the LLM's standard self-correction loop kick in without re-parsing inner JSON.
  - **Eval**: openab-agent only · drop-in (~10 LOC; plumb a flag from `meta_tool::dispatch` to caller) · **fit: in-scope**. High value-per-LOC.
- [x] **Row 518 — per-call timeout** — DONE: implemented via `PeerRequestOptions { timeout }` (not raw `tokio::time::timeout`) around both `tools/call` and `tools/list` in `meta_tool.rs`, default 60s, overridable per server via `request_timeout_secs` in `ServerConfig`. On timeout: `record_tool_call_outcome(server, false)` feeds `breaker.rs` + returns a `with_context` tool error. (See Section 1 rows 69-71 / Section 4 consolidate.)
  - **Eval**: openab-agent only · drop-in (~40 LOC + config field) · **fit: in-scope**. Low risk; protects against hung child processes.
- [ ] **Row 519 — `tracing::info!` audit log** at `call_tool` entry + exit (server, tool, arg sha256, duration_ms, outcome, is_error) using existing tracing plaintext fields per repo convention.
  - **Eval**: openab-agent only · drop-in (~15 LOC) · **fit: in-scope**. Free observability win; no new deps.
- [ ] **Rows 509/515/516 — HITL deny / confirm hook** for `call` action: optional async callback (ACP `tool_call_approval` frame) before `peer.call_tool`. Default = allow to preserve headless behaviour; UIs opt in.
  - **Eval**: hybrid (openab-agent + ACP adapter) · architectural commitment (~200 LOC + ACP frame schema design) · **fit: defer**. Needs ACP-side normative; track as design-doc placeholder.

## Server / Prompts

Source: [`server/prompts.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/prompts.mdx)

**Section-level finding**: openab-agent does **not** implement the prompts client surface at all. Conversation turns are produced by the LLM provider layer (`src/llm.rs` — `AnthropicProvider` / `OpenAiProvider` SDKs called directly), so MCP prompts never enter the agent's prompt pipeline. The MCP integration in `src/mcp/meta_tool.rs` exposes only `fetch_tools` + `call_tool` (`src/mcp/meta_tool.rs:51,71,98,116`), and the runtime initialises the rmcp client with the unit `()` `ClientHandler` blanket impl (`src/mcp/runtime.rs:1066,1079`), which inherits every default — including the no-op `on_prompt_list_changed` (SDK `handler/client.rs`). The rmcp 1.7.0 SDK *does* surface `Peer<RoleClient>::list_prompts` / `list_all_prompts` / `get_prompt` (SDK `service/client.rs:358-410`), but no call site in our tree invokes them. Most rows are therefore N/A by deliberate abstention; row 536 (client SHOULD paginate) is ❌ because we never list at all; row 527 (notification) is ⚠️ silently dropped by the default handler.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 520 | Servers with prompts MUST declare `prompts` capability | MUST | N/A | server-side declaration; openab-agent is client, no capability to publish |
| 521 | `prompts.listChanged` sub-capability | (capability) | N/A | server-side sub-capability; client advertises nothing here |
| 522 | `prompts/list` with pagination | (method) | N/A | server-side method; we never invoke `peer.list_prompts` (SDK `service/client.rs:359`) |
| 523 | Result: `prompts[]`, optional `nextCursor` | (field) | N/A | server-side response shape; never deserialised on our side |
| 524 | `prompts/get` request | (method) | N/A | server-side method; `peer.get_prompt` (SDK `service/client.rs:358`) never called |
| 525 | Result: `messages[]` (required), optional `description` | (field) | N/A | server-side response shape; `GetPromptResult` never constructed nor consumed |
| 526 | List-changed-capable servers SHOULD send `notifications/prompts/list_changed` | SHOULD | N/A | server-side emit obligation |
| 527 | `notifications/prompts/list_changed` notification | (notification) | ⚠️ | received via rmcp default `on_prompt_list_changed` (SDK `handler/client.rs`); silently no-ops because `()` handler at `src/mcp/runtime.rs:1066,1079` |
| 528 | Prompt fields: `name` (required) / `title` / `description` / `arguments` / `icons` (all optional) | (field) | N/A | schema field — we never produce or consume `Prompt` records |
| 529 | PromptMessage fields: role (user/assistant), content | (field) | N/A | schema field — `PromptMessage` never read; LLM turns come from `src/llm.rs` |
| 530 | Prompt content types: text / image / audio / resource | (field) | N/A | schema field — content variants never inspected on client side |
| 531 | Image content MUST be base64-encoded with valid MIME | MUST | N/A | server-side encoding obligation; we never decode prompt images |
| 532 | Audio content MUST be base64-encoded with valid MIME | MUST | N/A | server-side encoding obligation; we never decode prompt audio |
| 533 | Embedded resource MUST include valid URI, appropriate MIME, and text or blob | MUST | N/A | server-side embedding obligation; no embedded-resource consumer in tree |
| 534 | Servers SHOULD return `-32602` invalid prompt / missing args, `-32603` internal errors | SHOULD | N/A | server-side error contract; openab-agent never serves prompts |
| 535 | Servers SHOULD validate prompt arguments before processing | SHOULD | N/A | server-side validation duty; openab-agent has no prompts to validate |
| 536 | Clients SHOULD handle pagination for large prompt lists | SHOULD | ❌ | we never call `list_prompts` / `list_all_prompts` (SDK `service/client.rs:359,397`); pagination is moot — non-conformant by abstention. Mitigated by §1 below |
| 537 | Both parties SHOULD respect capability negotiation | SHOULD | ✅ | inherited from rmcp handshake at `src/mcp/runtime.rs:1066,1079`; `()` handler advertises no client capabilities, server `prompts` cap is parsed and simply unused |
| 538 | Implementations MUST validate prompt inputs/outputs to prevent injection | MUST | N/A | we never construct or render prompt messages; injection surface absent by abstention |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **§1. Decide whether to implement the prompts client surface.** Two options:
  - **(a) Implement** — wire `peer.list_all_prompts` + `peer.get_prompt` into `src/mcp/meta_tool.rs`, expose them on the ACP surface so Brett can pick a server-side prompt, then feed the resulting `PromptMessage[]` into `src/llm.rs` as system/user turns.
  - **(b) Stay tools-only and document abstention (recommended).** `src/llm.rs` already owns turn construction via Anthropic/OpenAI SDKs; MCP prompts are a server-driven *template* mechanism whose value proposition overlaps with what we already do natively. Adopting prompts forces a second turn-assembly path in the agent for marginal gain, and no current MCP server in our deployment publishes prompts worth surfacing.
  - **Eval**: docs only · docs only · **fit: in-scope**. Decision-only step; recommendation is (b) because integration cost is non-trivial and user-visible benefit is currently zero.
- [ ] **§2. If (a) wins: add `peer.list_all_prompts` + `peer.get_prompt` calls + ACP surface to expose prompts to Brett.** Add a `fetch_prompts(server)` analogue to `fetch_tools` (`src/mcp/meta_tool.rs:116`), plus a `get_prompt(server, name, args)` analogue to `call_tool` (`src/mcp/meta_tool.rs:71`). Surface both through the ACP meta-tool dispatch so Brett can list and instantiate prompts, then translate `GetPromptResult.messages` into the agent's internal turn representation before handing off to `src/llm.rs`.
  - **Eval**: openab-agent layer · non-trivial (~150-250 LOC across meta_tool / ACP schema / llm bridging + tests) · **fit: defer**. Architectural commitment to a parallel turn-assembly path; only worth it once a deployed MCP server actually publishes useful prompts.
- [ ] **§3. Override `on_prompt_list_changed`** from default no-op to a logged-cache-invalidation once §2 lands. Replace the unit `()` handler (`src/mcp/runtime.rs:1066,1079`) with a named struct (or reuse the one introduced for Section 10 §1 / Section 11 row 503) and implement `on_prompt_list_changed` to `tracing::info!` the event with the server identifier and invalidate the §2 cache. Upgrades row 527 from ⚠️ to ✅.
  - **Eval**: openab-agent layer · drop-in (~30 LOC; bundled with the named ClientHandler struct) · **fit: defer**. Mechanically trivial but pointless without §2's cache to invalidate.
- [ ] **§4. Document the deliberate abstention.** Land a short paragraph — either as the section-level note above (this audit) and/or as a `//!` doc-comment in `src/mcp/mod.rs` — stating that the prompts client surface is intentionally not implemented because LLM turn construction lives in `src/llm.rs` via native provider SDKs. Makes the N/A-by-abstention status discoverable from code without re-deriving it from `grep`.
  - **Eval**: docs only · docs only (~10 lines of prose) · **fit: in-scope**. Cheap, prevents future auditors from re-litigating the same question, and is the natural follow-through of recommending (b) in §1.

## Server / Resources

Source: [`server/resources.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/resources.mdx)

**Section-level finding**: openab-agent is purely a client and currently covers only the **tools** path of the MCP server surface. `src/mcp/meta_tool.rs` wires `fetch_tools` + `call_tool` only — server payloads reach Brett through `CallToolResult.content` (text / structured / embedded resource blocks). The **resources** surface — list / read / templates / subscribe — is the MCP equivalent of a file/blob browser, designed for clients that want to walk server-exposed URIs independent of a tool invocation. openab-agent does **not** wire any of it: zero functional hits for `list_resources` / `read_resource` / `ResourceTemplate` / `subscribe` in `src/`, and the `RunningService<RoleClient, ()>` blanket impl at `src/mcp/runtime.rs:1066,1079` means every `notifications/resources/updated` and `notifications/resources/list_changed` is silently no-op'd by `<() as ClientHandler>` (SDK `handler/client.rs`). rmcp 1.7.0 *does* expose `peer.list_resources` / `list_resource_templates` / `read_resource` / `subscribe` (SDK `service/client.rs:360-364`) plus `list_all_resources` pagination (`service/client.rs:413`), all uncalled. Most rows are therefore N/A by abstention; row 550 (resources/updated notification we'd receive) is ⚠️ silently dropped.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 539 | Servers with resources MUST declare `resources` capability | MUST | N/A | server-side declaration; we're a client (`src/mcp/runtime.rs:1056` `RunningService<RoleClient, ()>`) |
| 540 | `resources.subscribe` sub-capability (optional) | (capability) | N/A | server-declared capability; we never advertise (no server role) |
| 541 | `resources.listChanged` sub-capability (optional) | (capability) | N/A | server-declared capability; client side is rmcp default, no override |
| 542 | `resources/list` request with pagination | (method) | N/A | rmcp exposes `peer.list_resources` (SDK `service/client.rs:360`) — uncalled in our tree |
| 543 | Result: `resources[]`, optional `nextCursor` | (field) | N/A | server-produced; never consumed — no `list_resources` call site |
| 544 | `resources/read` with `uri` param | (method) | N/A | rmcp exposes `peer.read_resource` (SDK `service/client.rs:362`) — uncalled in `src/mcp/` |
| 545 | Result: `contents[]` | (field) | N/A | never invoked; `ResourceContents` (SDK `model/resource.rs:64`) unused |
| 546 | `resources/templates/list` request | (method) | N/A | rmcp `peer.list_resource_templates` (SDK `service/client.rs:361`) — uncalled |
| 547 | Result: `resourceTemplates[]` | (field) | N/A | server-produced; not consumed in our tree |
| 547a | ResourceTemplate fields: `uriTemplate` (required, RFC 6570) / `name` (required) / `title` / `description` / `mimeType` / `icons` (optional) | (field) | N/A | server-produced schema; SDK `RawResourceTemplate.uri_template: String` (SDK `model/resource.rs:45`) — no RFC 6570 parser in 1.7.0, see §4 |
| 548 | List-changed-capable servers SHOULD send `notifications/resources/list_changed` | SHOULD | N/A | server-side emit; if received, default `on_resource_list_changed` no-ops (SDK `handler/client.rs`) |
| 549 | `resources/subscribe` request | (method) | N/A | rmcp `peer.subscribe` (SDK `service/client.rs:363`) — uncalled; we never subscribe |
| 550 | `notifications/resources/updated` notification | (notification) | ⚠️ | we'd receive it but default `on_resource_updated` no-ops (SDK `handler/client.rs:215`); silently dropped (gated on subscribe path we don't have) |
| 551 | Resource fields: uri / name / title / description / mimeType / size / icons | (field) | N/A | server-produced field set; not consumed (no `read_resource` / `list_resources` calls) |
| 552 | Resource contents: text or base64 blob | (field) | N/A | `ResourceContents::{Text,Blob}ResourceContents` (SDK `model/resource.rs:66,75`) unused |
| 553 | Annotations (`audience` / `priority` / `lastModified`) apply to resources, resource templates, and content blocks | (field) | N/A | server-produced annotations; not consumed (no resources surface) |
| 554 | Servers SHOULD use `https://` only when client can fetch directly | SHOULD | N/A | server-side scheme choice |
| 555 | Servers SHOULD prefer another URI scheme (built-in or custom) when not directly web-fetchable | SHOULD | N/A | server-side scheme choice |
| 556 | MCP servers MAY use XDG MIME types (e.g. `inode/directory`) to identify non-regular `file://` resources without a standard MIME type | MAY | N/A | server-side MIME assignment |
| 556a | Standard URI schemes in spec: `https://`, `file://`, `git://` (servers MAY use custom schemes too) | (field) | N/A | server-side scheme choice |
| 557 | Custom URI schemes MUST conform to RFC 3986 | MUST | N/A | server-side URI production; we never produce URIs for resources |
| 558 | Servers SHOULD return `-32002` resource-not-found, `-32603` internal | SHOULD | N/A | server-side error mapping; we never call `read_resource` so the code path is unreachable |
| 559 | Servers MUST validate resource URIs | MUST | N/A | server-side validation |
| 560 | Access controls SHOULD be implemented for sensitive resources | SHOULD | N/A | server-side access control |
| 561 | Binary data MUST be properly encoded | MUST | N/A | server-side encoding (base64 blob payload); we don't consume |
| 562 | Resource permissions SHOULD be checked before operations | SHOULD | N/A | server-side authz |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **§1. Decide on resources surface — (a) implement vs (b) stay tools-only (recommended).** The tool-result path (`CallToolResult.content` with `EmbeddedResource` blocks) already lets MCP servers return resource-like payloads in-band when a tool call asks for them; that matches our ACP UX where Brett issues an instruction and gets a streamed reply. A standalone "browse server's resource tree" surface only pays off when there's an interactive picker on the ACP client (Zed) and a user mental model of "the server has a filesystem I want to mount." Adding it now expands attack surface (untrusted URIs, base64 blobs) without a concrete product driver.
  - **Eval**: docs only · docs only · **fit: in-scope**. Punt with a written abstention so the next reviewer doesn't re-litigate.
- [ ] **§2. If (a) wins: wire `peer.list_resources` + `peer.read_resource` to ACP.** Add a `resources_meta_tool.rs` mirroring `meta_tool.rs`: `list_resources` / `read_resource` / `list_resource_templates` calling the existing rmcp `Peer<RoleClient>` methods (SDK `service/client.rs:360-362`), then surface results as ACP tool blocks (text → text content, blob → base64 attached as `EmbeddedResource`). Cursor pagination wrapper `list_all_resources` already exists in SDK (`service/client.rs:413`).
  - **Eval**: openab-agent layer · non-trivial (~150-250 LOC + ACP surfaces + auth-scope review for `file://`) · **fit: defer**. Wait for a real ask from Brett or a concrete server we want to browse.
- [ ] **§3. If (a) + subscribe: wire `on_resource_updated` from no-op to ACP push.** Replace the `()` blanket impl in `src/mcp/runtime.rs:1066,1079` with a named struct (bundled with Section 10 §1, Section 11 row 503, Section 12 §3) that overrides `on_resource_updated` (SDK `handler/client.rs:215`) and `on_resource_list_changed` and routes the URI into the ACP session as a notification block. Needs an ACP-side rendering decision (toast? inline?) plus per-subscription bookkeeping.
  - **Eval**: openab-agent layer · architectural commitment (touches handler type, session-state map, ACP notification protocol) · **fit: defer**. Only meaningful after §2 lands.
- [ ] **§4. File rmcp upstream tracker for 1.7.0 schema-as-string gaps.** Two concrete gaps observed: (i) `RawResourceTemplate.uri_template` is `String` with no RFC 6570 parser / validator (SDK `model/resource.rs:45`); (ii) `RawResource.uri` is `String` with no RFC 3986 typing. Both would bite §2 immediately.
  - **Eval**: rmcp upstream · drop-in (issue + ask-bullets) · **fit: defer**. File only when §2 promotes from defer → in-scope; today it's a hypothetical pain point.
- [ ] **§5. Document the abstention.** Drop a one-paragraph note in this section preamble (already added above as section-level finding) and cross-reference §1; keeps the N/A column self-explanatory for future reviewers and prevents a "why is the whole section blank?" round-trip.
  - **Eval**: docs only · docs only (~6 lines) · **fit: in-scope**. Cheap; ships the decision into the canonical doc; no code risk. Effectively already satisfied by this audit pass.

## Completion

Source: [`server/utilities/completion.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/completion.mdx)

**Section-level finding**: openab-agent does NOT invoke `completion/complete`. We are a meta-tool gateway — the LLM constructs full `arguments` JSON for `tools/call`, so there is no "user is typing into an argument field" surface that completion suggestions would feed. rmcp 1.7.0 ships the full client-side surface (`CompleteRequestParams` model `model.rs:2222-2231`, `peer.complete()` request method generated at `service/client.rs:356` via `method!`, convenience `peer.complete_prompt_argument` `service/client.rs:463-482`, `peer.complete_resource_argument` `service/client.rs:494-...`, and `CompletionInfo::MAX_VALUES = 100` enforced in `model.rs:2280`) — but no callsite exists in `src/mcp/**` (grep `complete\b` in `src/mcp/*.rs` returns only OAuth `complete_login` / `verification_uri_complete` matches, all unrelated). All client SHOULDs (rows 574-576) are vacuously satisfied; server rows are off-surface.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 563 | Servers supporting completions MUST declare `completions` capability | MUST | N/A | server-side normative; we are an MCP client |
| 564 | `completion/complete` request | (method) | N/A | we never invoke. rmcp ships `peer.complete(CompleteRequestParams)` (SDK `service/client.rs:356` via `method! peer_req complete`) + convenience methods `complete_prompt_argument` (`service/client.rs:463`) / `complete_resource_argument` (`service/client.rs:494`); zero callsite in `src/mcp/**` |
| 565 | Params: `ref`, `argument`, optional `context.arguments` | (field) | N/A | params authoring surface; rmcp `CompleteRequestParams { meta, ref, argument, context: Option<CompletionContext> }` (SDK `model.rs:2222-2231`) carries all three with `_meta` per SEP-1319 |
| 565a | Clients SHOULD include previously-resolved arguments in `context.arguments` for multi-argument refs | SHOULD | N/A (vacuously) | we send no completion requests; `CompletionContext` (SDK `model.rs:2179`) is wired into rmcp via the `with_context` builder (`model.rs:2245`) when/if invoked |
| 566 | Reference types: `ref/prompt`, `ref/resource` | (field) | N/A | schema authoring surface; rmcp `Reference::for_prompt` / `Reference::for_resource` constructors used by the convenience methods (SDK `service/client.rs:472, 503`) |
| 567 | Result: `completion.values`, optional `total`, `hasMore` | (field) | N/A | server-side result schema; rmcp `CompletionInfo { values: Vec<String>, total: Option<u32>, has_more: Option<bool> }` (SDK `model.rs:2270-2276`); we never receive |
| 568 | Max 100 items per completion response | (constraint) | N/A | server obligation; rmcp enforces SDK-side via `CompletionInfo::MAX_VALUES = 100` const + validation in `CompletionInfo::new` (SDK `model.rs:2280-2290`) returning `Err` on overflow |
| 569 | Servers SHOULD return `-32601` (capability not supported), `-32602` (invalid prompt name / missing required args), `-32603` (internal error) | SHOULD | N/A | server-emit obligation |
| 570 | Servers SHOULD return suggestions sorted by relevance | SHOULD | N/A | server obligation |
| 571 | Servers SHOULD implement fuzzy matching where appropriate | SHOULD | N/A | server obligation |
| 572 | Servers SHOULD rate-limit completion requests | SHOULD | N/A | server obligation |
| 573 | Servers SHOULD validate all inputs | SHOULD | N/A | server obligation |
| 574 | Clients SHOULD debounce rapid completion requests | SHOULD | N/A (vacuously) | we issue zero completion requests — no rapid-fire stream to debounce |
| 575 | Clients SHOULD cache completion results where appropriate | SHOULD | N/A (vacuously) | nothing to cache |
| 576 | Clients SHOULD handle missing/partial results gracefully | SHOULD | N/A (vacuously) | we receive no results; if/when wired, `CompleteResult.completion` exposes `total: Option<u32>` / `has_more: Option<bool>` (SDK `model.rs:2273-2275`) so partial states are already typed in |
| 577 | Implementations MUST validate completion inputs | MUST | N/A | server obligation (sender-side validation is server's burden when responding to `completion/complete`) |
| 578 | Implementations MUST implement appropriate rate limiting | MUST | N/A | server obligation |
| 579 | Implementations MUST control access to sensitive suggestions | MUST | N/A | server obligation (don't leak sensitive identifiers via suggestion stream) |
| 580 | Implementations MUST prevent info disclosure via completions | MUST | N/A | server obligation (don't leak existence of restricted prompts / resources via suggestion presence) |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Section-level disposition**: defer the entire surface until a client-side use case exists. Today's meta-tool flow (`src/mcp/meta_tool.rs`) routes `tools/list` + `tools/call` only — the LLM produces complete `arguments` JSON, there is no human-typing-an-argument moment to suggest into.

- [ ] **§1. Document the abstention.** Add a one-paragraph note in this section's preamble (already added above as section-level finding) cross-referencing Sections 12 / 13 (prompts / resources — also unused on our side). Keeps the N/A column self-explanatory.
  - **Eval**: docs only · drop-in (~6 lines, already inline above) · **fit: in-scope**. Cheap; ships the decision into the canonical doc; no code risk. Effectively already satisfied by this audit pass.
- [ ] **§2. Future — `prompts/resources` UI surface.** Iff openab-agent ever grows a prompts catalog (Section 12) or resource picker (Section 13) that exposes an argument-typing surface, wire `peer.complete_prompt_argument(prompt, arg, partial, ctx)` / `peer.complete_resource_argument(uri_template, arg, partial, ctx)` from `rmcp::service::client` directly — the convenience methods already round-trip context-argument carry-through (SEP-1320 / row 565a). Pair with `(a)` debounce, `(b)` LRU cache, `(c)` `has_more`-aware paging UI.
  - **Eval**: hybrid (rmcp ships convenience methods + 100-item cap; openab-agent owns ACP/UI plumbing + debounce + cache) · architectural commitment (no UI today, so this is a green-field surface; ~200-300 LOC if combined with §2 of Section 12) · **fit: defer**. Only meaningful if Section 12 / 13 ship a UI surface; today the LLM-driven dispatch model makes completion a no-op.
- [ ] **§3. (Tracking only) — `_meta` carry-through.** If §2 lands, ensure SEP-1319 `_meta` is preserved across the request: rmcp `CompleteRequestParams.meta: Option<Meta>` field (SDK `model.rs:2225`) already supports it via `RequestParamsMeta` trait impl (`model.rs:2251`). No code change today; flag for §2 reviewers.
  - **Eval**: rmcp upstream · docs only · **fit: defer**. Tracking note for the §2 ship date.

## Logging

Source: [`server/utilities/logging.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/logging.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 581 | Servers emitting log notifications MUST declare `logging` capability | MUST | N/A | server-side normative; we are an MCP client. `ServerCapabilities.logging: Option<JsonObject>` (rmcp `model/capabilities.rs:308`) — parsed but unused |
| 582 | Log levels follow RFC 5424 (debug, info, notice, warning, error, critical, alert, emergency) | (field) | N/A | server-produced field. rmcp `LoggingLevel` enum exposes all 8 levels for incoming notifications (SDK `model.rs:1450-1459`) |
| 583 | `logging/setLevel` request | (method) | N/A | server-side handler obligation. rmcp ships `SetLevelRequestParams` + `SetLevelRequest` (SDK `model.rs:1467-1496`); not invoked in `src/mcp/**` |
| 584 | Clients MAY send `logging/setLevel` | MAY | ❌ | rmcp provides `peer.set_level(SetLevelRequestParams)` (SDK `service/client.rs:357`); zero callsite in `src/mcp/**`. Capability exists; deferred — see §2 |
| 584a | Server MAY automatically decide log level if no `logging/setLevel` request has been received from the client (per `schema.mdx` JSDoc on `LoggingMessageParams`) | MAY | N/A | server-side policy; we never call `peer.set_level` so server defaults always apply |
| 585 | `notifications/message` with level / logger / data | (notification) | N/A | server-produced. rmcp `LoggingMessageNotificationParam` (SDK `model.rs:1504-1512`) routed to `on_logging_message`; `()` blanket impl at `src/mcp/runtime.rs:1066,1079` uses default no-op (SDK `handler/client.rs:208-214`) |
| 586 | Servers SHOULD return `-32602` invalid level, `-32603` internal errors | SHOULD | N/A | server-side error mapping; we never invoke `set_level` |
| 587 | Servers SHOULD rate-limit log messages | SHOULD | N/A | server-side rate-limit policy |
| 588 | Servers SHOULD include context in `data` field | SHOULD | N/A | server-side field population |
| 589 | Servers SHOULD use consistent logger names | SHOULD | N/A | server-side logger naming convention |
| 590 | Servers SHOULD remove sensitive info | SHOULD | N/A | server-side content filtering obligation |
| 591 | Clients MAY present / filter / persist log messages | MAY | ⚠️ | rmcp wires `LoggingMessageNotificationParam` to handler trait; `()` impl drops every notification via default no-op (SDK `handler/client.rs:208-214`). MAY satisfied vacuously — see §1 |
| 592 | Log messages MUST NOT contain credentials/secrets | MUST NOT | N/A | server-side authorship obligation; we relay unmodified from peer |
| 593 | Log messages MUST NOT contain PII | MUST NOT | N/A | server-side authorship obligation |
| 594 | Log messages MUST NOT contain internal details aiding attacks | MUST NOT | N/A | server-side authorship obligation |
| 595 | Implementations SHOULD rate-limit, validate data, control log access, monitor for sensitive content | SHOULD | N/A | server-side implementation obligation; client-side we receive unsanitized notification and default-drop |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Section-level disposition**: openab-agent has no observability surface for server-emitted logs today. rmcp 1.7.0 ships the full client plumbing (`LoggingMessageNotificationParam`, `on_logging_message` trait hook, `peer.set_level`, `LoggingLevel` RFC 5424 enum), but the `()` ClientHandler blanket impl drops every notification. All server-side rows are N/A by topology; client MAY rows (584, 591) are deferred pending an ops driver.

- [ ] **§1. Replace `()` ClientHandler with a named struct that tees server logs into local `tracing`.** Override `on_logging_message` to map `LoggingLevel` → `tracing::{error,warn,info,debug,trace}!` and emit `(server, logger, level)` as plaintext fields per repo convention. Do NOT propagate the `data` field contents (log the *fact* a message arrived plus byte size, not the payload) to avoid transitive secret leakage if a server is compromised (row 590 is aspirational). Bundle with Section 10 §1 / Section 11 row 503 / Section 12 §3 — they all need the same named-handler refactor.
  - **Eval**: openab-agent layer · drop-in (~40-60 LOC; bundled with named ClientHandler) · **fit: in-scope (bundled)**. Cheap once the handler struct lands; gives ops visibility into upstream server failures (e.g., "tool X polling timed out") without LLM round-trips.
- [ ] **§2. If §1 ships: wire `peer.set_level` from connect-time config.** Add an optional `logging.level` field to `ServerConfig` (`src/mcp/config.rs`); when populated, parse to `LoggingLevel` and call `peer.set_level(SetLevelRequestParams::new(level))` in `Dial::run` after handshake. Upgrades row 584 from ❌ to ✅.
  - **Eval**: openab-agent layer · drop-in (~30 LOC + config schema bump) · **fit: in-scope (gated on §1)**. Free observability dial once §1's handler exists; pointless without it.
- [ ] **§3. Rate-limiting on client side.** Defer — server-side SHOULD (row 587) already covers the producer; if a peer floods us, the named handler from §1 can add a token bucket later, but no concrete bug today.
  - **Eval**: openab-agent layer · non-trivial (~80 LOC + per-server bucket state) · **fit: defer**. Wait for a real noisy server before paying the complexity.
- [ ] **§4. Document the deferral.** Add a one-paragraph note in this section's preamble describing the §1 abstention + the planned `OPENAB_LOG_LEVEL` style env var if §2 ships. Cross-reference Section 17 (Trust / Safety) on secret-scrubbing.
  - **Eval**: docs only · drop-in (~6 lines) · **fit: in-scope**. Cheap; prevents future auditors from re-deriving the same question.

## Pagination

Source: [`server/utilities/pagination.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/pagination.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 596 | Cursor-based pagination model | (model) | ✅ | rmcp `Cursor` type alias for `String` (SDK `model.rs`); paginated result types carry `next_cursor: Option<Cursor>` via the `paginated_result!` macro |
| 597 | Clients MUST NOT assume fixed page size | MUST NOT | ✅ | `peer.list_all_tools` (SDK `service/client.rs:378-392`) loops on `next_cursor` with no page-size assumption; openab-agent calls it at `src/mcp/meta_tool.rs:122` |
| 598 | Response includes optional `nextCursor` | (field) | N/A | server-produced field; client-side rmcp deserialises into `next_cursor: Option<Cursor>` on all `*List` result types |
| 599 | Request includes optional `cursor` | (field) | ✅ | rmcp `PaginatedRequestParams { meta, cursor: Option<String> }` (SDK `model.rs`); auto-passed through by `list_all_tools` |
| 600 | Paginated ops: resources/list, resources/templates/list, prompts/list, tools/list | (method) | ⚠️ | tools-only on our side: `peer.list_all_tools` (`src/mcp/meta_tool.rs:122`). Prompts / resources / resource templates vacuously compliant — no client callsite (Sections 12 / 13 N/A by topology) |
| 601 | Servers SHOULD provide stable cursors | SHOULD | N/A | server-side cursor lifecycle obligation |
| 602 | Servers SHOULD handle invalid cursors gracefully | SHOULD | N/A | server-side error handling |
| 603 | Clients SHOULD treat missing `nextCursor` as end of results | SHOULD | ✅ | rmcp `list_all_tools` loop breaks when `next_cursor.is_none()` (SDK `service/client.rs:378-392`); same pattern in `list_all_prompts` / `list_all_resources` |
| 604 | Clients SHOULD support both paginated and non-paginated flows | SHOULD | ✅ | `list_all_tools` wraps the low-level `list_tools(Some(PaginatedRequestParams { .. }))` — servers returning a single page (zero `next_cursor`) complete in one iteration without special-casing |
| 605 | Clients MUST treat cursors as opaque tokens | MUST | ✅ | cursors stored as `Option<String>` and passed through unmodified by rmcp; openab-agent never inspects them |
| 605a | Clients MUST NOT make assumptions about cursor format | MUST NOT | ✅ | `Cursor = String` alias with no parser / pattern match in our tree; treated as black box |
| 606 | Clients MUST NOT parse or modify cursors | MUST NOT | ✅ | `list_all_tools` consumes `result.next_cursor` and feeds it verbatim into the next `PaginatedRequestParams.cursor`; zero string ops in client code |
| 607 | Clients MUST NOT persist cursors across sessions | MUST NOT | ✅ | cursor lifetime = single `list_all_tools` call stack (local `mut cursor = None`); no serialisation to disk / cache. Each `start_mcp_session` re-issues fresh `list_all_tools(cursor=None)` |
| 608 | Invalid cursors SHOULD result in `-32602` Invalid params | SHOULD | N/A | server-side error mapping; rmcp surfaces wire `Err(-32602)` to caller unchanged but we never trigger it (we don't synthesise cursors) |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Section-level disposition**: pagination compliance is complete for tools (the only paginated surface openab-agent actively consumes) and vacuously compliant for prompts / resources / resource templates (uncalled — see Sections 12 / 13). All client-side MUST / MUST NOT rows pass; SHOULDs are satisfied by rmcp SDK contract. Server rows are N/A by client topology. Recommendations are forward-hardening rather than corrective.

- [x] **§1. Document cursor opacity invariant in `src/mcp/meta_tool.rs`.** Added a comment above the `fetch_tools` pagination loop's `let mut cursor = None;` (`src/mcp/meta_tool.rs`) noting the cursor is an opaque server token, round-tripped verbatim, and never parsed / synthesized / persisted — reinforces rows 605-607 for future maintainers tempted to cache pagination state.
  - **Eval**: docs only · drop-in (~3 lines) · **fit: in-scope**. Cheap defensive doc; near-zero risk.
- [ ] **§2. If Sections 12 / 13 ever ship the prompts / resources client surface (§2 of each), re-audit pagination there.** Today rows 600 for prompts / resources are vacuously compliant; once `peer.list_all_prompts` / `peer.list_all_resources` callsites land we must re-verify the same MUST NOT-persist / opaque-token invariants apply to the new code paths.
  - **Eval**: openab-agent layer · drop-in (audit / re-check) · **fit: defer (bundled)**. Free follow-up tied to whichever section ships first.
- [ ] **§3. (Tracking only) — rmcp SDK regression guard for cursor opacity.** rmcp 1.7.0 ships `Cursor = String` with no `impl FromStr` / `Display::format` parser; a future SDK version could regress by typing the cursor (e.g. base64 wrapper). No code change today; if such a change lands we re-evaluate rows 605/605a/606.
  - **Eval**: rmcp upstream · docs only · **fit: defer**. Tracking note only.

## Trust, Safety & Consent (Key Principles)

Source: [`index.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/index.mdx)

The "Security and Trust & Safety" section of the spec is meta-governance — lowercase prose "must/should" forms principles (not BCP 14 normative), and the "Implementation Guidelines" subsection has 5 explicit **SHOULD** items. The protocol cannot enforce these at wire level, but implementations are tracked here for completeness.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 609 | Users must explicitly consent to and understand all data access and operations | (principle) | N/A | Host responsibility (ACP host above openab-agent enforces); see §6 |
| 610 | Users must retain control over what data is shared and what actions are taken | (principle) | N/A | Host responsibility (ACP); see §6 |
| 611 | Implementors should provide clear UIs for reviewing and authorizing activities | (principle) | N/A | Host responsibility — openab-agent is headless / no UI surface |
| 612 | Hosts must obtain explicit user consent before exposing user data to servers | (principle) | N/A | Host boundary — ACP host enforces before server connection; openab-agent reads `mcp.json` server list it was handed |
| 613 | Hosts must not transmit resource data elsewhere without user consent | (principle) | N/A | Host policy layer — openab-agent relays MCP calls per Host instruction |
| 614 | User data should be protected with appropriate access controls | (principle) | ⚠️ | partial: path validation on file tools (`src/tools.rs:12-55`); ACP host owns runtime isolation. openab-agent has no per-user data segregation |
| 615 | Tools represent arbitrary code execution and must be treated with appropriate caution | (principle) | ⚠️ | acknowledged via circuit breaker on MCP failures (`src/mcp/breaker.rs`, wired into the manager at `src/mcp/runtime.rs:137-141`) — no per-call human-in-the-loop consent gate; see §1 / §7 |
| 616 | Descriptions of tool behavior (annotations) should be considered untrusted unless from a trusted server | (principle) | ❌ | annotations dropped before LLM sees them: `list_tools` / `describe_tool` projection (`src/mcp/meta_tool.rs:139-161`) returns `{name, description[, input_schema]}` only — `annotations` from `rmcp::model::Tool` silently stripped (see also row 508). De-facto "untrusted" by omission but no explicit trust model documented — see §1 |
| 617 | Hosts must obtain explicit user consent before invoking any tool | (principle) | ❌ | no per-tool consent gate in openab-agent: `call_tool` (`src/mcp/meta_tool.rs:95-98`) dispatches via `peer.call_tool` directly; agent routing (`src/agent.rs:184` ish) executes LLM tool calls without user approval. ACP host can gate above us but openab-agent provides no hook today — see §2 / §7 |
| 618 | Users should understand what each tool does before authorizing its use | (principle) | ⚠️ | partial: `describe_tool` provides `input_schema` but not `annotations` (`ToolAnnotations::{read_only_hint, destructive_hint, idempotent_hint, open_world_hint}`). Risk hints are not surfaced — see §1 |
| 619 | Users must explicitly approve any LLM sampling requests | (principle) | N/A | vacuously compliant: no `create_message` override; client-side sampling capability not advertised in `ClientCapabilities` — see §3 |
| 620 | Users should control: whether sampling occurs at all, the actual prompt sent, what results the server can see | (principle) | N/A | vacuously compliant: sampling not enabled client-side — see §3 |
| 621 | Implementors SHOULD build robust consent and authorization flows | SHOULD | ❌ | not implemented — see row 617 gap. openab-agent layer lacks per-tool approval mechanism; ACP host responsible for consent UI — see §7 |
| 622 | Implementors SHOULD provide clear documentation of security implications | SHOULD | ⚠️ | partial: ADR comments (`src/mcp/meta_tool.rs:1-5`, `src/mcp/runtime.rs:1-13`) explain phase scope; no explicit security documentation of annotation trust model or per-call consent design — see §5 |
| 623 | Implementors SHOULD implement appropriate access controls and data protections | SHOULD | ⚠️ | partial: path validation (`src/tools.rs`), circuit breaker on MCP failures (`src/mcp/breaker.rs`). Structured audit log NOT implemented — `record_tool_call_outcome` (`src/mcp/runtime.rs` ish) logs breaker state, not per-call operation trail — see §2 |
| 624 | Implementors SHOULD follow security best practices in their integrations | SHOULD | ⚠️ | partial: PKCE in OAuth (`src/mcp/flow.rs`), token refresh single-flight gate (`src/mcp/runtime.rs:131-136`). No secret scrubbing on tracing / audit path — see §4 |
| 625 | Implementors SHOULD consider privacy implications in their feature designs | SHOULD | ⚠️ | partial: idle eviction (`src/mcp/runtime.rs` background task) limits long-term data retention; no per-user segregation; secret handling relies on user discretion — see §4 / §6 |
| 626 | Host enforces security policies and consent requirements | (role) | N/A | Host role (from `architecture/index.mdx` Core Components); ACP host above openab-agent |
| 627 | Host handles user authorization decisions | (role) | N/A | Host role (from `architecture/index.mdx` Core Components); ACP host above openab-agent |
| 628 | Host controls client connection permissions and lifecycle | (role) | N/A | Host role (from `architecture/index.mdx` Core Components); ACP host above openab-agent |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Section-level disposition**: openab-agent sits below an ACP Host. Spec rows 609-613 / 619-620 / 626-628 are Host obligations — N/A at this layer. Rows 614-618 (principles) and 621-625 (implementor SHOULDs) are openab-agent's responsibility, partially satisfied. Primary gaps: (1) annotations not surfaced through `describe_tool` (rows 616, 618), (2) no structured per-tool-call audit log (rows 621, 623), (3) no secret-scrubbing pass on tracing / audit emission (row 624), (4) no per-tool consent hook for ACP Host to wire into (rows 615, 617). Sampling rows (619, 620) are vacuously N/A by design — capability is not advertised.

- [ ] **§1. Surface tool annotations in `list_tools` + `describe_tool` projection.** Add `annotations` (with sub-fields `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint`, plus the annotation-level `title`) and the top-level `Tool.title` field to the JSON response in `src/mcp/meta_tool.rs:139-161` so the LLM (and any future ACP-side consent UI) can route on risk hints. Upgrades rows 616 / 618 from ❌ / ⚠️ to ⚠️ / ✅ (annotations remain untrusted by spec — surfacing makes the trust posture explicit instead of dropping silently). Bundle with Section 11 row 484 — same projection edit.
  - **Eval**: openab-agent only · drop-in (~25 LOC; projection-only) · **fit: in-scope**. Bundle with the Section 11 Improvement Plan for tools.
- [ ] **§2. Structured per-tool-call audit log line.** Add `tracing::info!(target = "openab_agent::mcp::audit", server, tool, args_sha256, duration_ms, is_error, ...)` at `peer.call_tool` entry + exit in `src/mcp/meta_tool.rs:98-109`. Plaintext tracing fields per repo convention (no JSON, no metrics crate). Lets the ACP Host or sidecar tap the audit stream — openab-agent emits the event, Host decides where it goes. Upgrades rows 621 / 623 from ❌ / ⚠️ to ⚠️ / ✅. Bundle with Section 11 row 519.
  - **Eval**: openab-agent only · drop-in (~15 LOC; same as Section 11 row 519) · **fit: in-scope (bundled)**. Free observability win.
- [ ] **§3. Document the sampling abstention (rows 619-620).** Drop a one-paragraph note in this section's preamble explaining that openab-agent advertises no `ClientCapabilities { sampling: .. }` today and the `()` ClientHandler default `create_message` returns `Err(-32601)`. If a future Host wants sampling, they must (a) advertise the capability, (b) override `create_message` to gate the request through ACP user-approval before forwarding to the LLM provider layer in `src/llm.rs`. Cross-reference Section 9 (Client / Sampling) audit if it exists.
  - **Eval**: docs only · drop-in (~8 lines) · **fit: in-scope**. Cheap; makes vacuous compliance intentional rather than accidental.
- [ ] **§4. Secret scrubbing on audit / tracing path (row 624).** Add an optional `redact_secrets()` pass on the audit-log fields from §2 that masks values matching patterns (`api_key`, `bearer`, `password`, `token`, `secret`, `AKIA[0-9A-Z]{16}`, etc.) before tracing emit. Pattern list configurable via env / `redact.toml`; default-on. Pair with row 590 (Logging) which is server-side aspirational — we cannot enforce on inbound logs, but we can enforce on our own audit emissions.
  - **Eval**: openab-agent layer · drop-in (~30-40 LOC + pattern config) · **fit: in-scope (gated on §2)**. Cheap defensive measure; non-blocking — Host can layer its own pass on top.
- [ ] **§5. Document Host / Agent / Server boundary in section preamble.** Land a paragraph clarifying: "Rows 609-613 / 619-620 / 626-628 are Host obligations (ACP host above openab-agent); rows 614-618 are principles openab-agent contributes mechanisms toward; rows 621-625 are implementor SHOULDs owned by openab-agent." Upgrades row 622 ⚠️ → ✅ and prevents future auditors from re-litigating the layering.
  - **Eval**: docs only · drop-in (~10 lines) · **fit: in-scope**. Critical for next review pass; near-zero risk.
- [ ] **§6. Per-tool consent hook for ACP Host wiring (rows 615 / 617 / 621).** Introduce an optional async callback `ToolCallApprovalFn` invoked before `peer.call_tool` in `src/mcp/meta_tool.rs:98`. Default = `Allow` (preserves current headless behaviour). ACP adapter (`src/acp.rs`) can register a callback that emits an ACP `tool_call_approval` frame and awaits Host response. Pair with §1 so the approval prompt carries annotation risk hints.
  - **Eval**: hybrid (openab-agent + ACP adapter; needs ACP frame schema design) · architectural commitment (~200 LOC + protocol-level review) · **fit: defer**. Track as design-doc placeholder; bundle with Section 11 §HITL deny / confirm (rows 509 / 515 / 516). Not blocking until a Host actually asks for the gate.
- [ ] **§7. Sampling capability gating tracker.** When (if) Section 9 ships a sampling client surface, this row's compliance flips from N/A vacuous to require explicit user approval (rows 619 / 620). Today's N/A is correct; the tracker prevents accidentally enabling sampling without consent flow.
  - **Eval**: openab-agent layer · drop-in (audit tag in this row) · **fit: defer**. Tracking only.

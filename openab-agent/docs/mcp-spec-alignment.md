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
| SDK | _TBD_ |
| Last refreshed | 2026-06-03 |

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
| 19 | Implementations MUST validate schemas according to declared/default dialect | MUST | ❌ | no schema validation in `openab-agent` — tool input schemas passed through to LLM as-is (`src/mcp/meta_tool.rs:95-116`); confirmed by Mira via `Cargo.lock` audit |
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
| 37b | JSON-RPC `Error.message` SHOULD be concise single sentence | SHOULD | ⚠️ | wrapped via `anyhow::Context` + `format!`; multi-clause sites confirmed: `src/mcp/runtime.rs:1753` (HTTP error body format), `src/mcp/config.rs:155-157` (spec config `read_to_string` + `serde_json::from_str` with two `with_context` layers), `src/mcp/config.rs:170-173` (env var resolve via `interpolate_value` + `with_context`), `src/mcp/meta_tool.rs:95+` (tool-call params), `src/mcp/oauth.rs:57,63` (PKCE/OAuth `anyhow!` sites) — Mira-extended inventory; Jelly fact-check 2026-06-03 dropped stale `runtime.rs:272` (not an error site, struct-literal line) + repointed `config.rs:149-150` → `155-157`, `config.rs:166` → `170-173` |

### Improvement Plan (Jelly + Mira consensus, section 0)

- [ ] **Rows 18-21 (JSON Schema 2020-12)**: rmcp 1.7.0 confirmed not to validate dialects (no `jsonschema` / `valico` in `Cargo.lock`; only `schemars` for gen + `serde` for deserialize). Decide: (a) add a thin validator at `src/mcp/meta_tool.rs::fetch_tools` boundary using `jsonschema` crate, OR (b) document this as a known limitation in README and surface unsupported-dialect tool entries as `NeedsAttention`. Either way, document supported dialect in `openab-agent/docs/`.
  - **Eval**: openab-agent layer (rmcp doesn't own schema-dialect validation and probably shouldn't — it's a serialization SDK) · option (a) drop-in with `jsonschema` crate (~100 LOC) but compliance theatre because we pass schemas straight to the LLM which tolerates any dialect; option (b) docs-only drop-in · **fit: borderline — recommend (b)**. Adding a validator for tool-schemas we just relay isn't a meaningful gate. Score: lean to docs-only.
- [ ] **Rows 19-20**: surface unsupported-dialect tool entries as `NeedsAttention` rather than silently passing through; covers MUST-handle-gracefully.
  - **Eval**: openab-agent only (extends our existing `ServerStatus` model, mirrors `NeedsAuth`) · drop-in (~30 LOC) · **fit: in-scope**. Even if we skip row 18 validation, surfacing "we don't know this dialect" is the bare-minimum graceful handling the MUST asks for.
- [ ] **Row 37b (`Error.message` brevity)**: introduce a `concise_error_message(err: &anyhow::Error) -> String` helper that takes the top-level cause's `to_string()` (not the chained one) for the JSON-RPC error payload, and keep the full chain only in `tracing` logs. Audit sites: `runtime.rs:1753`, `config.rs:155-157`, `config.rs:170-173`, `meta_tool.rs:95+`, `oauth.rs:57,63`.
  - **Eval**: openab-agent only · drop-in (one helper + call-site touch-ups, ~80 LOC) · **fit: low-value in-scope**. SHOULD, not MUST. anyhow chains are arguably useful debug context; only do this if a real server / LLM complains about verbose error.messages. Score: defer until pain.
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
| 59 | Server capability: `tools` | (capability) | N/A | server-side; we consume via `peer.list_all_tools()` at `src/mcp/meta_tool.rs:123` + `peer.call_tool()` at `src/mcp/meta_tool.rs:98` |
| 60 | Server capability: `logging` | (capability) | N/A | server-side |
| 61 | Server capability: `completions` | (capability) | N/A | server-side |
| 62 | Server capability: `tasks` | (capability) | N/A | server-side |
| 63 | Server capability: `experimental` | (capability) | N/A | server-side |
| 64 | Both parties MUST respect the negotiated protocol version | MUST | ✅ | `rmcp` SDK |
| 65 | Both parties MUST only use successfully negotiated capabilities | MUST | ⚠️ | we only call `tools/list` + `tools/call`; we do NOT gate on server-advertised capabilities — assume `tools` cap is always present, no check before `meta_tool.rs:123` invocation |
| 66 | stdio shutdown: client SHOULD close stdin, wait, SIGTERM, then SIGKILL | SHOULD | ❌ | no explicit shutdown sequence; relies on `Drop` of `RunningService` + `TokioChildProcess` which does not implement the spec-recommended graceful termination ladder |
| 67 | Server MAY initiate stdio shutdown | MAY | N/A | server-side |
| 68 | HTTP shutdown by closing associated HTTP connection(s) | (transport) | ✅ | `rmcp::transport::StreamableHttpClientTransport` connection lifecycle (drop) |
| 69 | Implementations SHOULD establish timeouts on all sent requests | SHOULD | ❌ | no `tokio::time::timeout` wrapping `peer.call_tool().await` (`meta_tool.rs:98`) or `peer.list_all_tools().await` (`meta_tool.rs:122`); circuit breaker (`src/mcp/breaker.rs`) is failure-rate based, not per-request timeout |
| 70 | On timeout, sender SHOULD issue a cancellation notification | SHOULD | ❌ | no cancellation; `src/acp.rs:91-92` has `TODO(v0.2): implement cancellation token to abort in-progress agent.run()` |
| 71 | SDKs/middleware SHOULD allow per-request timeout configuration | SHOULD | ❌ | no API surface; `McpConfig` (`src/mcp/config.rs`) has no timeout fields |
| 72 | MAY reset timeout clock on progress notification | MAY | N/A | no timeout to reset |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 51 (HTTP `MCP-Protocol-Version` header)**: verify `rmcp::transport::StreamableHttpClientTransport` injects the header on every subsequent request after handshake; if not, add via `auth_header()`-style injection on transport config (`runtime.rs:1073-1077`). Document the verification in alignment doc.
  - **Eval**: rmcp already handles this (Section 2 audit confirmed `protocol_headers` injection in SDK `transport/streamable_http_client.rs:408-418, 511-522`). · docs-only drop-in · **fit: in-scope**. Just record the verification in the alignment doc + a one-line note in `runtime.rs` callsite; no code change.
- [ ] **Rows 52-56 (Client capabilities)**: decide minimum viable client capability set. Priority: (a) `roots` for filesystem-style servers that constrain by directory (most common server need); (b) `sampling` if/when we want servers to delegate LLM calls back to us; (c) `elicitation` for interactive UX. Implement via rmcp `ClientHandler` trait instead of `()`.
  - **Eval**: rmcp `ClientHandler` trait fully supports these (the unit `()` impl is a deliberate "no capabilities" sentinel) · drop-in for `roots` (wrapper struct + 1 method to expose `ServerConfig`-declared roots, ~80 LOC) · architectural commitment for `sampling`/`elicitation` (would need to wire back to the host LLM + ACP prompt UI — wrapper-sized, not drop-in) · **fit: in-scope for `roots`, borderline for `sampling`/`elicitation`**. Recommend ship `roots` first; defer the other two until a real server use case asks.
- [ ] **Row 65 (Capability gating)**: before calling `peer.list_all_tools()` / `peer.call_tool()`, inspect `peer.peer_info()?.capabilities.tools` — if absent, fail with a clear `ServerStatus::Failed("server does not advertise tools capability")` instead of letting rmcp surface a generic JSON-RPC error.
  - **Eval**: openab-agent only (rmcp exposes `peer.peer_info()` returning the cached `InitializeResult`) · drop-in (~20 LOC at `meta_tool.rs` boundary) · **fit: in-scope**. Cheap defensive check, better error message; aligns with our `ServerStatus` model.
- [ ] **Row 66 (stdio shutdown ladder)**: implement explicit shutdown in `ServerHandle::disconnect()` — (1) close child stdin handle (drop sender), (2) `tokio::time::timeout(grace, child.wait())`, (3) on timeout `child.kill()` (SIGTERM via signal-hook or `Child::start_kill()`), (4) final SIGKILL on second grace expiry. Default grace 5s + 5s.
  - **Eval**: rmcp `TokioChildProcess` does NOT expose stdin-close-then-graceful-wait ladder — it relies on `Drop` which is ungraceful (SDK `transport/child_process.rs:23-200`) · openab-agent wrapper (~60 LOC) OR upstream rmcp PR adding `shutdown(grace)` method · **fit: in-scope as wrapper**. Spec is SHOULD, not MUST, but rude `SIGKILL`-on-drop has cost servers their state in the field. Wrapper is cleaner than waiting on upstream.
- [ ] **Rows 69-71 (Request timeouts)**: add `request_timeout_secs` field per-server in `McpConfig::Stdio` / `McpConfig::Http` (`src/mcp/config.rs`), default 60s. Wrap every `peer.call_tool()` / `peer.list_all_tools()` site in `tokio::time::timeout(...)`. Pair with row 70.
  - **Eval**: openab-agent only · drop-in (~40 LOC config field + 2 callsite wraps) · **fit: in-scope**. Pure tokio idiom, no rmcp involvement; complements existing `breaker.rs` failure-rate logic with per-request bound.
- [ ] **Row 70 (Cancellation notification on timeout)**: when the timeout from rows 69-71 fires, emit `notifications/cancelled` via rmcp's built-in auto-cancel path. Also unblocks `acp.rs:91-92` TODO for `session/cancel`.
  - **Eval (corrected 2026-06-03)**: rmcp 1.7.0 `RequestHandle::await_response` (SDK `service.rs:322-343`) ALREADY auto-emits `CancelledNotification` with `reason="request timeout"` when `PeerRequestOptions.timeout` expires — request-id threading is internal to rmcp · openab-agent drop-in is just switching `peer.call_tool(p).await` to `peer.send_request_with_option(req, opt_with_timeout).await?.await_response().await` (~30-50 LOC unified with rows 69-71) · **fit: in-scope, drop-in**. Prior eval (~120 LOC, non-trivial) was wrong — rmcp ships this pattern. See Section 4 Improvement Plan for consolidated treatment.
| 73 | Implementations SHOULD always enforce a maximum timeout (even with progress) | SHOULD | ❌ | no max-timeout ceiling; same gap as rows 69-71 (no `tokio::time::timeout` wrap at `meta_tool.rs:98, 123`). Jelly fact-check 2026-06-03 filled |
| 74 | Implementations SHOULD handle version mismatch, capability failures, timeouts | SHOULD | ⚠️ | partial: version mismatch ✅ via rmcp `serve()` returning `Err` + our `with_context` wrap at `runtime.rs:1068,1081` → `ServerStatus::Failed`; capability failure ❌ (row 65, no gating); timeout ❌ (rows 69-71). Jelly fact-check 2026-06-03 filled |
| 74a | `Implementation` object (clientInfo / serverInfo) carries optional `title`, `description`, `icons`, `websiteUrl` fields | (schema) | ✅ | `rmcp::model::Implementation` (1.7.0 schema) carries these via `serde(skip_serializing_if = "Option::is_none")`; we use bare default `Implementation` via `()` handler so we don't populate them — but the field-presence requirement is rmcp's responsibility (SDK side). Jelly fact-check 2026-06-03 filled |

## Transports

Source: [`basic/transports.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 75 | JSON-RPC messages MUST be UTF-8 | MUST | ✅ | `serde_json` produces UTF-8; rmcp `AsyncRwTransport` (SDK `rmcp::transport::async_rw`) serializes via `serde_json::to_string` |
| 76 | Clients SHOULD support stdio whenever possible | SHOULD | ✅ | `Dial::Stdio` via `rmcp::transport::TokioChildProcess` (`src/mcp/runtime.rs:1064`); enabled via Cargo feature `transport-child-process` (`Cargo.toml:26`) |
| 77 | stdio messages delimited by newlines; MUST NOT contain embedded newlines | MUST NOT | ✅ | rmcp `AsyncRwTransport` uses newline-delimited framing (`rmcp::transport::async_rw` LinesCodec-style reader/writer); `serde_json::to_string` produces single-line JSON (no embedded `\n`) |
| 78 | Server MAY write UTF-8 to stderr for any logging (including non-error) | MAY | N/A | server-side |
| 79 | Client MAY capture/forward/ignore server's stderr | MAY | ⚠️ | rmcp `TokioChildProcess` defaults `stderr=Stdio::inherit()` (SDK `rmcp::transport::child_process`), so child stderr flows to our process stderr; we don't capture per-server. Acceptable for "forward" semantics |
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
| 101 | Client MUST respect SSE `retry` field, waiting that many ms before reconnect | MUST | ⚠️ | rmcp `client_side_sse` tracks `server_retry_interval: Option<Duration>` (SDK `transport/common/client_side_sse.rs:135, 151`) — assumed honored by SSE retry loop. Needs verification that the field is actually applied vs. only the local `retry_policy` |
| 102 | SSE stream SHOULD eventually include the JSON-RPC response for the originating request | SHOULD | N/A | server-side |
| 103 | Server MAY send other requests/notifications on SSE before the response | MAY | N/A | server-side |
| 104 | Pre-response messages SHOULD relate to originating request | SHOULD | N/A | server-side |
| 105 | Server MAY terminate SSE stream if session expires | MAY | N/A | server-side |
| 106 | After response sent, server SHOULD terminate SSE stream | SHOULD | N/A | server-side |
| 107 | Disconnection MAY occur at any time | MAY | N/A | observational |
| 108 | Disconnection SHOULD NOT be interpreted as request cancellation | SHOULD NOT | ✅ | rmcp client treats SSE disconnect as a transient stream event → triggers reconnect (`retry_connection`), not request abort |
| 109 | To cancel, client SHOULD send `CancelledNotification` | SHOULD | ❌ | no client-side cancellation surface; `src/acp.rs:91-92` TODO + no `peer.notify_cancelled()` callsites in `src/mcp/` |
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
| 139 | Client MUST include `MCP-Protocol-Version: <protocol-version>` header on all HTTP requests | MUST | ✅ | rmcp worker extracts `init_result.protocol_version` from `InitializeResult` and injects `mcp-protocol-version` header into `protocol_headers` used for all subsequent POSTs (SDK `transport/streamable_http_client.rs:408-418, 511-522`). GET uses `custom_headers` separately — see Improvement Plan |
| 140 | Sent protocol-version header value SHOULD be the negotiated one | SHOULD | ✅ | uses `init_result.protocol_version.as_str()` directly (SDK `transport/streamable_http_client.rs:413, 515`) |
| 141 | If server receives no `MCP-Protocol-Version` header and has no other way to identify the version (e.g., via initialization negotiation), it SHOULD assume `2025-03-26` | SHOULD | N/A | server-side |
| 142 | If invalid/unsupported `MCP-Protocol-Version` is sent, server MUST respond HTTP 400 | MUST | N/A | server-side; rmcp surfaces as `UnexpectedServerResponse` |
| 143 | Implementations MAY implement custom transports | MAY | N/A | we don't implement custom transports — stdio + Streamable HTTP only (`src/mcp/runtime.rs:1042-1053`) |
| 144 | Custom transports MUST preserve JSON-RPC + lifecycle | MUST | N/A | no custom transports |
| 145 | Custom transports SHOULD document connection establishment / message exchange patterns | SHOULD | N/A | no custom transports |
| 145a | Client MAY implement legacy HTTP+SSE backwards-compat flow: POST `InitializeRequest`; on HTTP 400/404/405 fall back to GET expecting `endpoint` SSE event (for interop with 2024-11-05 HTTP+SSE servers) | MAY | N/A (intentional) | conscious decision (Brett 2026-06-03) to **not** implement legacy 2024-11-05 HTTP+SSE compatibility. rmcp 1.7.0 client doesn't fall back; init failure against a legacy-only server surfaces as `UnexpectedServerResponse`. Servers MUST upgrade to Streamable HTTP to be supported |
| 145b | Servers wanting to support older clients SHOULD continue to host both the SSE and POST endpoints of the old transport, alongside the new MCP endpoint | SHOULD | N/A | server-side |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 79 (stderr capture)**: optionally capture per-server child stderr (via `rmcp::transport::child_process::TokioChildProcess::stderr(Stdio::piped())`) and tee into our `tracing` log with `target=mcp.<server_name>.stderr`. Lets operators see `npx mcp-server-*` startup failures without losing them in container stderr noise. Low priority unless Brett wants per-server log isolation.
  - **Eval**: rmcp `TokioChildProcess::builder()` exposes the `stderr(Stdio)` setter (SDK `transport/child_process.rs`), so the plumbing exists · openab-agent drop-in (~50 LOC: pipe stderr, spawn tokio task that reads lines + emits `tracing::info!(target=..., line=%line)`) · **fit: in-scope**. Operator-quality-of-life win; matches our existing tracing-only observability rule.
- [ ] **Row 90 (`Accept` header order)**: cosmetic — spec lists `application/json, text/event-stream`, rmcp emits `text/event-stream, application/json`. Order is non-normative per RFC 7231; document this in alignment doc as acceptable rather than file an rmcp PR.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Already noted in row 90 cell. No code change needed; RFC 7231 §5.3.2 makes order non-significant for Accept.
- [ ] **Row 101 (SSE `retry` field honoured)**: verify that rmcp's `client_side_sse::server_retry_interval` is actually applied to the reconnect delay (and overrides `retry_policy` per spec MUST). If not, file rmcp upstream issue; meanwhile flag this as known soft-gap.
  - **Eval**: rmcp layer (the retry-loop scheduler lives in SDK `transport/common/client_side_sse.rs`) · investigation first (~30 min reading), then either zero code (if honoured) or upstream rmcp PR (if not) · **fit: in-scope as investigation**. We can't easily wrapper around this — it's deep in the SSE reconnect loop. If gap confirmed, upstream is the right place.
- [ ] **Row 109 (CancelledNotification)**: implement `notifications/cancelled` emission alongside Section 1's request timeout work (rows 69-71) and `acp.rs:91-92` TODO. Unified cancellation surface that timeout + `session/cancel` ACP method both route through.
  - **Eval**: dup of Section 1 Row 70 eval — rmcp `Peer::notify_cancelled` exists but request-id threading is non-trivial · openab-agent wrapper (~120 LOC, shared with Section 1 Row 70) · **fit: in-scope but defer until rows 69-71 land**. Single implementation covers both rows.
- [ ] **Row 139 (GET stream protocol header)**: confirm `get_stream` carries `MCP-Protocol-Version` — the worker passes `protocol_headers` into POST loops but `get_stream` accepts a `custom_headers` map separately. If rmcp doesn't merge `protocol_headers` into the GET call, the spec MUST is violated for the server-initiated SSE path. If gap is real, upstream-fix in rmcp (cleanest) or build a wrapper that re-injects via `custom_headers`.
  - **Eval**: rmcp layer (the header-merge happens in SDK `transport/streamable_http_client.rs` worker plumbing) · investigation first, then either zero code OR upstream rmcp PR OR openab-agent custom-headers workaround (~20 LOC) · **fit: in-scope as investigation**. This is a real MUST so worth the dig; if gap confirmed, upstream PR preferable but custom_headers passthrough is a viable workaround until merged.

## Authorization

Source: [`basic/authorization.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/authorization.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 146 | Authorization is OPTIONAL | OPTIONAL | ✅ | per-server `oauth: Option<OAuthConfig>` (`src/mcp/config.rs:33`); HTTP servers can run anonymous (`runtime.rs:1077`) |
| 147 | HTTP transports SHOULD conform to authorization spec | SHOULD | ⚠️ | OAuth 2.1 PKCE flow + bearer header (`src/mcp/oauth.rs`, `src/mcp/flow.rs`, `auth.rs:351-410`) — base mechanics conform; key MCP-spec gaps in PRM (RFC 9728), AS-metadata discovery dual-mechanism (RFC 8414+OIDC), CIMD, RFC 8707 resource param — see rows below |
| 148 | STDIO SHOULD NOT follow this spec; credentials from environment | SHOULD NOT | ✅ | stdio uses `env_clear()` + explicit `envs(stdio_child_env(&env))` (`src/mcp/runtime.rs:1059-1063`); no OAuth on stdio path |
| 149 | Alternative transports MUST follow established security best practices for their protocol | MUST | N/A | no alternative transports beyond stdio + Streamable HTTP |
| 150 | Authorization servers MUST implement OAuth 2.1 with appropriate security measures for both confidential and public clients | MUST | N/A | AS-side |
| 151 | AS and MCP clients SHOULD support OAuth Client ID Metadata Documents | SHOULD | ❌ | no CIMD client support; we use pre-registered client IDs only (env-injected via `OPENAB_MCP_<provider>_CLIENT_ID`, `oauth.rs:188-209`) |
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
| 169 | Clients supporting all registration options SHOULD prefer pre-registered, then CIMD, then DCR, then prompt | SHOULD | ⚠️ (degenerate) | we only support pre-registered (env-injected client IDs) — top of the priority list is honoured, the rest don't exist |
| 170 | MCP clients and AS SHOULD support OAuth Client ID Metadata Documents | SHOULD | ❌ | duplicate of row 151; no CIMD |
| 171 | CIMD-supporting MCP implementations MUST follow OAuth CIMD requirements | MUST | N/A | no CIMD support — vacuously satisfied |
| 172 | CIMD: clients MUST host metadata document at HTTPS URL per RFC requirements | MUST | N/A | no CIMD |
| 173 | CIMD: `client_id` URL MUST use `https` scheme with a path component | MUST | N/A | no CIMD |
| 174 | CIMD: metadata MUST include at least `client_id`, `client_name`, `redirect_uris` | MUST | N/A | no CIMD |
| 175 | CIMD: clients MUST ensure `client_id` value matches the document URL exactly | MUST | N/A | no CIMD |
| 176 | CIMD: clients MAY use `private_key_jwt` for client authentication | MAY | N/A | no CIMD |
| 177 | CIMD: MCP clients SHOULD check for `client_id_metadata_document_supported` AS capability | SHOULD | N/A | no CIMD |
| 178 | CIMD: MCP clients MAY fall back to DCR or pre-registration if CIMD unavailable | MAY | ⚠️ | we _always_ use pre-registration; this is "fall back to" by virtue of having no CIMD or DCR — vacuous |
| 178a | CIMD (AS-side): AS SHOULD fetch metadata documents when encountering URL-formatted `client_id`s | SHOULD | N/A — client-side | (AS-side) |
| 178b | CIMD (AS-side): AS MUST validate fetched document's `client_id` matches the URL exactly | MUST | N/A — client-side | (AS-side) |
| 178c | CIMD (AS-side): AS SHOULD cache metadata respecting HTTP cache headers | SHOULD | N/A — client-side | (AS-side) |
| 178d | CIMD (AS-side): AS MUST validate redirect URIs in authorization request against metadata document | MUST | N/A — client-side | (AS-side) |
| 178e | CIMD (AS-side): AS MUST validate metadata document structure is valid JSON and contains required fields | MUST | N/A — client-side | (AS-side) |
| 179 | Pre-registration: MCP clients SHOULD support an option for static client credentials | SHOULD | ✅ | env-injected client ID per built-in provider (`oauth.rs::builtin_client_id`, env var pattern `OPENAB_MCP_<provider>_CLIENT_ID`); custom providers carry `client_id: Option<String>` on `OAuthConfig` (`config.rs:85`) |
| 180 | MCP clients and AS MAY support RFC 7591 Dynamic Client Registration | MAY | ❌ | no DCR |
| 181 | Scope Selection: clients SHOULD follow least privilege when requesting scopes | SHOULD | ⚠️ | per-built-in `default_scopes` baked in (`oauth.rs::ProviderSpec`); custom providers carry user-supplied `scopes` (`config.rs:79`) — no enforcement that the set is least-privilege, but defaults are deliberately minimal (e.g. Linear `read`-set) |
| 182 | Scope Selection: clients SHOULD prefer `scope` from initial `WWW-Authenticate` header, else `scopes_supported` from PRM, else omit `scope` | SHOULD | ❌ | no challenge-driven scope selection |
| 183 | MCP clients MUST implement RFC 8707 Resource Indicators (`resource` parameter) | MUST | ❌ | no `resource` parameter on authorize URL — `flow.rs:48-50` appends `code_challenge`, `code_challenge_method`, no `resource`; auth.rs PKCE flow also lacks it (auth.rs:357) |
| 184 | `resource` parameter MUST be included in both authorization and token requests | MUST | ❌ | not implemented in either |
| 185 | `resource` parameter MUST identify the intended MCP server | MUST | ❌ | not implemented |
| 186 | `resource` MUST use the canonical URI per RFC 8707 §2 | MUST | ❌ | not implemented |
| 187 | MCP clients SHOULD provide the most specific URI possible for the MCP server | SHOULD | ❌ | not implemented |
| 188 | Implementations SHOULD accept uppercase scheme/host for robustness | SHOULD | N/A | no `resource` canonicalization since none sent |
| 189 | Implementations SHOULD consistently use no-trailing-slash form for interoperability | SHOULD | N/A | no `resource` canonicalization since none sent |
| 190 | MCP clients MUST send `resource` regardless of AS support | MUST | ❌ | not implemented |
| 191 | Access token handling MUST conform to OAuth 2.1 §5 | MUST | ✅ | tokens stored on disk via `save_namespaced_token_at` (`auth.rs:33`), retrieved via `load_namespaced_token_at`; bearer-injected into transport via `auth_header(token)` (`src/mcp/runtime.rs:1073-1074`) |
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
| 210 | Implementations MUST follow OAuth 2.1 §7 Security Considerations | MUST | ⚠️ | PKCE + state nonce ✅, but missing: RFC 8707 resource audience binding (rows 183-190), `WWW-Authenticate`-driven reauth (row 164) |
| 211 | MCP clients MUST include `resource` parameter for audience binding | MUST | ❌ | dup of 183/190 — not implemented |
| 212 | MCP servers MUST validate tokens are issued for their own use | MUST | N/A | server-side |
| 213 | Clients and servers MUST implement secure token storage per OAuth 2.1 §7.1 | MUST | ⚠️ | tokens written to `auth.json` under `~/.openab/agent/` via `auth.rs` (filesystem-permissioned to user); not encrypted at rest, not in OS keyring. Adequate for a single-user agent on a single-user host but not "secure storage" in the strong sense |
| 214 | AS SHOULD issue short-lived access tokens | SHOULD | N/A | AS-side |
| 215 | For public clients, AS MUST rotate refresh tokens | MUST | N/A | AS-side; client-side: refresh-token handling lives in `auth.rs` token-refresh path |
| 216 | Implementations MUST follow OAuth 2.1 §1.5 Communication Security | MUST | ✅ | all auth flow URLs are HTTPS in built-in `ProviderSpec`s (`oauth.rs:14-30`); custom providers have unenforced URL scheme — see Improvement Plan |
| 217 | All AS endpoints MUST be HTTPS | MUST | ⚠️ | built-ins ✅; custom provider `authorize_url` / `token_url` not validated as `https://` — see Improvement Plan |
| 218 | All redirect URIs MUST be localhost or HTTPS | MUST | ⚠️ | built-ins pin `http://localhost:<port>/callback` (acceptable per spec localhost exception); custom `redirect_uri` not validated — see Improvement Plan |
| 219 | MCP clients MUST implement PKCE per OAuth 2.1 §7.5.2 | MUST | ✅ | `generate_pkce()` + `code_challenge_method=S256` in `src/mcp/flow.rs:42-50` (paste-back) and `src/auth.rs:351, 357` (browser); verifier preserved across paste-back via `PendingPasteLogin` (`runtime.rs:269-275`) and sent to token endpoint (`auth.rs:410, 493, 585`) |
| 220 | MCP clients MUST verify PKCE support before proceeding with authorization | MUST | ❌ | no `code_challenge_methods_supported` check; PKCE is unconditionally sent regardless of AS metadata (which we don't fetch — see row 156) |
| 221 | MCP clients MUST use `S256` code challenge method when technically capable (OAuth 2.1 §4.1.1) | MUST | ✅ | unconditional `S256` (`flow.rs:50`, `auth.rs:357`) |
| 222 | OAuth 2.0 AS metadata: if `code_challenge_methods_supported` absent, clients MUST refuse to proceed | MUST | ❌ | no AS-metadata fetch (row 156) so we can't enforce this; we send PKCE anyway, which is the safe behaviour but technically violates the "refuse to proceed" wording when metadata is absent |
| 223 | OIDC Discovery 1.0: clients MUST verify `code_challenge_methods_supported` is present; refuse if absent | MUST | ❌ | as above |
| 224 | AS providing OIDC Discovery 1.0 MUST include `code_challenge_methods_supported` | MUST | N/A | AS-side |
| 225 | MCP clients MUST have redirect URIs registered with the AS | MUST | ✅ | built-ins pin `callback` per `ProviderSpec` (`oauth.rs:18, 23+`), pre-registered with each AS; custom flow requires `redirect_uri` field |
| 226 | AS MUST validate exact redirect URIs against pre-registered values | MUST | N/A | AS-side |
| 227 | MCP clients SHOULD use and verify `state` parameter, discard mismatches | SHOULD | ✅ | `state` generated + verified per `flow.rs::init_paste_authorize` (random nonce) + `flow.rs::complete_paste_authorize` (state echo check per RFC 6749 §10.12 — see runtime.rs:299 comment); state snapshot in `PendingPasteLogin` (`runtime.rs:271`) |
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
| 242 | MCP clients MUST implement and use the RFC 8707 `resource` parameter (aligns with RFC 9728 §7.4 recommendation) | MUST | ❌ | dup of 183-190; not implemented |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Rows 153, 154, 156, 159, 164-168 (PRM + AS-metadata discovery)**: implement the spec-required client discovery surface — (a) on initial 401, parse `WWW-Authenticate` for `resource_metadata` and `scope`; (b) GET `/.well-known/oauth-protected-resource{path}` to fetch PRM; (c) follow `authorization_servers` to RFC 8414 / OIDC `.well-known/oauth-authorization-server` + `openid-configuration` (with the priority order in rows 167-168); (d) populate `authorize_url`, `token_url`, `code_challenge_methods_supported` from the discovered metadata when `OAuthConfig.discovery=true`. Existing `discovery_allowlist` SSRF guard (`config.rs:104-110`) becomes load-bearing instead of decorative. Largest single MCP-spec gap right now.
  - **Eval**: rmcp 1.7.0 ships `auth` feature with `AuthorizationManager` (in `rmcp::transport::auth`) which already implements RFC 8414/OIDC + PRM discovery — but `runtime.rs` doesn't use it; it threads bearer tokens directly via `auth_header()`. Switching to `AuthorizationManager` (or adopting its discovery sub-modules) is the right path · wrapper-sized refactor in openab-agent (~300-500 LOC: replace pre-resolved `authorize_url`/`token_url` with discovery-on-401, plumb through `discovery_allowlist` SSRF guard) · **fit: in-scope** (this is exactly what rmcp's auth feature is for; we're reinventing it badly). Largest single payoff for spec MUST compliance.
- [ ] **Rows 183-190, 211, 242 (RFC 8707 `resource` parameter)**: add `resource=<canonical-server-URI>` to (a) authorize URL builder in `flow.rs:46-50` + `auth.rs:357`; (b) token-exchange request body in `auth.rs:410, 493, 585`. Source URI = `ServerConfig::Http.url` canonicalised per RFC 8707 §2 (lowercase scheme/host, strip default port, strip trailing slash). MUST in spec; audience-binding gap is the biggest security delta after PRM.
  - **Eval**: openab-agent only (rmcp `AuthorizationManager` may surface a `resource` setter once we adopt it; until then we own the param injection) · drop-in (~60 LOC: canonical URI helper + 5 callsite injections) · **fit: in-scope**. Pure URL-builder work, MUST-level spec compliance, no architectural commitments. Highest ROI security item.
- [ ] **Rows 161-162, 164, 207, 207a (`WWW-Authenticate` parsing + step-up reauth)**: when rmcp surfaces `StreamableHttpError::AuthRequired` / `InsufficientScope` (already carrying `required_scope`), trigger reauth flow with the challenge-provided scope set rather than bubbling up. Hook in `meta_tool.rs` tool-call path. Couples with row 183-190 (resource param needs to be re-sent on step-up).
  - **Eval**: rmcp already does the heavy lifting (parses `WWW-Authenticate` and surfaces structured `AuthRequired { www_authenticate }` / `InsufficientScope { required_scope }` errors per SDK `transport/common/reqwest/streamable_http_client.rs:136-166`) · openab-agent drop-in (~100 LOC: catch the two error variants in `meta_tool.rs`, route to existing OAuth flow with new scope set, retry once) · **fit: in-scope**. We're already half-built — rmcp surfaces what we need, just not consumed yet. Pairs naturally with PRM discovery.
- [ ] **Rows 220, 222, 223 (PKCE methods verification)**: once AS-metadata discovery lands, check `code_challenge_methods_supported` contains `S256` before issuing the request; abort with clear error if absent. Until discovery is done, document the "always send PKCE, trust the AS" behaviour as a known soft-violation.
  - **Eval**: openab-agent only, blocked on PRM/AS-metadata discovery work (rows 153-168) · drop-in once unblocked (~15 LOC: existence check + clear error) · **fit: in-scope**. Trivial given discovery; meaningless until then. Document as known gap meanwhile.
- [ ] **Rows 217, 218 (HTTPS / localhost enforcement for custom providers)**: tighten `OAuthConfig::validate` to reject non-`https://` `authorize_url` / `token_url` and non-`localhost`/non-`https` `redirect_uri` for custom providers. Trivial 10-line addition in `src/mcp/config.rs:104-112`.
  - **Eval**: openab-agent only · drop-in (~15 LOC scheme check + tests) · **fit: in-scope**. MUST-level spec compliance, near-zero implementation cost, no rmcp coupling. Ship this first as cheap quick-win.
- [ ] **Row 213 (secure token storage)**: optionally back `auth.json` with an OS keyring (`keyring` crate) when available; fall back to filesystem mode. Low priority unless we hear of a leak vector; current model is adequate for single-user dev hosts.
  - **Eval**: openab-agent only (rmcp leaves token persistence to consumer) · wrapper-sized (~150 LOC: feature-gated `keyring` crate, fallback path, cross-platform testing on Linux/macOS) · **fit: borderline — defer**. SHOULD spec; openab-agent's main deploy targets are containers (no OS keyring) so fallback is the common case anyway. Score: defer unless a leak vector forces it.
- [ ] **Documentation**: `openab-agent/docs/` should call out (a) the PRM / RFC 8707 gap with explicit "what works without spec compliance" matrix, (b) supported / unsupported registration mechanisms (pre-registered only, no CIMD, no DCR), (c) which built-in providers exist and which env vars wire their client IDs.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Sets reader expectation honestly; matches our docs-first culture. Cheap.

## Cancellation

Source: [`basic/utilities/cancellation.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/cancellation.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 243 | `notifications/cancelled` carries `requestId` and optional `reason` | (notification) | ✅ (schema) | rmcp `CancelledNotificationParam { request_id, reason: Option<String> }` (SDK `service.rs:333-336`) — both fields present per schema |
| 244 | Cancellation notification MUST only reference requests issued in the same direction | MUST | N/A (vacuous) | we send no `notifications/cancelled` today (no `notify_cancelled` callsite in `src/mcp/**`); if/when we do via rmcp `peer.notify_cancelled` (SDK `service/client.rs:368`) the direction will be client→server, matching same-direction requirement |
| 245 | Cancellation notification MUST only target requests believed in-progress | MUST | N/A (vacuous) | no cancel emission today. Future: rmcp `RequestHandle::cancel` (SDK `service.rs:349-360`) drops the handle's receiver after sending — naturally only references in-progress request id |
| 246 | `initialize` request MUST NOT be cancelled by clients | MUST NOT | ✅ | initialize is performed inside `().serve(transport).await` at `runtime.rs:1066,1079`; rmcp owns the request id and never exposes it externally, so we cannot cancel it even if we wanted to |
| 247 | For task-augmented requests, the `tasks/cancel` request MUST be used instead of the `notifications/cancelled` notification (tasks have a dedicated cancellation that returns final state) | MUST | N/A | we do not use task-augmented requests (no `tasks` client capability per Section 1 row 55); `tasks/cancel` is the task transport's surface, not ours |
| 248 | Receivers SHOULD stop processing, free resources, not respond | SHOULD | N/A | we never receive `notifications/cancelled` from server — client handler is bare `()` (no `on_cancelled` impl); rmcp default discards via the `ClientHandler` blanket impl (SDK `handler/client.rs:46`) |
| 249 | Receivers MAY ignore cancellation if request unknown / complete / uncancellable | MAY | N/A | as above; we are effectively the "ignore" path by virtue of having no handler |
| 250 | Sender SHOULD ignore any late response to a cancelled request | SHOULD | N/A (vacuous) | no cancellations sent today. Future: rmcp `RequestHandle::cancel` drops the response channel `rx` so any late response is discarded by the transport worker (SDK `service.rs:323-326`) |
| 251 | Both parties MUST handle cancel race conditions gracefully | MUST | N/A (vacuous) | no cancellations sent. rmcp's auto-cancel-on-timeout (SDK `service.rs:332-343`) is race-safe (`rx` consumed before notification sent); inheriting this behaviour requires the `PeerRequestOptions { timeout }` work in Section 1 rows 69-71 |
| 252 | Both parties SHOULD log cancellation reasons | SHOULD | ❌ | no cancellation emission; when implemented (Section 1 rows 69-70) we'd need `tracing::info!(target="mcp.cancel", server=%name, reason=%r)` at the emission site. rmcp internally already logs via the `tracing` crate but our reason wrappers aren't visible at our layer today |
| 253 | Application UIs SHOULD indicate cancellation state | SHOULD | N/A | openab-agent is a CLI/meta-tool gateway — no UI surface. ACP `session/cancel` (`acp.rs:91-92`) is the closest surface but only a transport hook, not a UI |
| 254 | Invalid cancellation notifications SHOULD be ignored | SHOULD | ✅ | rmcp `ClientHandler` default `on_cancelled` discards unknown notifications (no panic / error propagation); we inherit this via `()` handler — bare `()` impl satisfies the SHOULD by routing unknown notifications to no-op |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

**Major finding (worth highlighting)**: rmcp 1.7.0 already implements the full cancel-on-timeout pattern internally — `RequestHandle::await_response` (SDK `service.rs:322-343`) auto-emits `CancelledNotification` with `reason="request timeout"` when `PeerRequestOptions.timeout` expires. We just don't use the option today (our `peer.call_tool(params).await` path doesn't go through the option-bearing API). This collapses Section 1 rows 69-71 + row 70 + Section 4 row 252 into a single switch: route `call_tool` via `peer.send_request_with_option(req, PeerRequestOptions::new().with_timeout(d))` then `await_response().await`.

- [ ] **Row 252 (Log cancellation reasons)**: when adopting rmcp's auto-cancel-on-timeout (see major finding above), add a `tracing::info!(target="mcp.cancel", server=%name, request_id=?id, reason=%reason)` log at the openab-agent layer immediately after the timeout fires. rmcp itself does not log the cancellation at info level.
  - **Eval**: openab-agent only (rmcp emits the notification on its own but doesn't expose a hook for application-level logging without rebuilding the path) · drop-in (~10 LOC if we own the `send_request_with_option` wrapper) · **fit: in-scope**. Trivial extension of the timeout work; matches our existing tracing-only observability rule.
- [ ] **(consolidate with Section 1 rows 69-71 + row 70)**: switching `peer.call_tool(params).await` at `meta_tool.rs:98` and `peer.list_all_tools().await` at `meta_tool.rs:123` to the option-bearing path gives us, for free, (a) per-request timeout, (b) automatic `CancelledNotification` emission with `reason="request timeout"`, (c) race-safe response discard. This is a single ~50 LOC refactor that satisfies the entire timeout+cancel column.
  - **Eval**: rmcp covers the heavy lifting (`PeerRequestOptions::with_timeout`, `RequestHandle::await_response`, auto-cancel emission) · openab-agent drop-in (replace `.call_tool(p).await` with `send_request_with_option(req, opt).await?.await_response().await`, plus a small param→request adapter) · **fit: in-scope, high value**. Previous Section 1 eval for Row 70 (called it "non-trivial, ~120 LOC") was WRONG — rmcp ships this pattern out of the box. Correcting that eval as part of this improvement.

## Progress

Source: [`basic/utilities/progress.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/progress.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 255 | `progressToken` carried in request `_meta` | (field) | ❌ | we never populate `_meta.progressToken` on outbound `peer.call_tool(params)` — `CallToolRequestParams::new(...).with_arguments(...)` at `meta_tool.rs:95-98` only sets `name` + `arguments`. So no server can stream progress back to us today |
| 256 | Progress tokens MUST be string or integer | MUST | ✅ (schema) | `rmcp::model::ProgressToken(pub NumberOrString)` (SDK `model.rs:305`) — `NumberOrString` enforces string-or-integer at the type level |
| 257 | Progress tokens MUST be unique across active requests | MUST | N/A (vacuous) | we send no `progressToken` (row 255); if implemented, would need a per-`McpRuntimeManager` token counter or UUID source |
| 258 | `notifications/progress` carries token, progress, optional total/message | (notification) | ✅ (schema) | `rmcp::model::ProgressNotificationParam { progress_token, progress: f64, total: Option<f64>, message: Option<String> }` (SDK `model.rs:1100-1115`); method const `notifications/progress` |
| 259 | `progress` value MUST increase with each notification, even if total is unknown | MUST | N/A | we send no progress; on receive we discard via `()` handler. The SDK doc-comment on `progress: f64` matches the spec wording (SDK `model.rs:1107-1108`) but rmcp does not enforce monotonicity on either send or receive — it's the application's burden |
| 260 | `progress` and `total` MAY be float | MAY | ✅ (schema) | both fields are `f64` in rmcp schema (SDK `model.rs:1107, 1110`) |
| 261 | `message` field SHOULD provide relevant human-readable progress information | SHOULD | N/A | we send no progress; rmcp provides `with_message(impl Into<String>)` builder (SDK `model.rs:1134`) |
| 262 | Progress notifications MUST only reference active in-progress operation tokens | MUST | N/A (vacuous) | we send no progress; if implemented, drop the token when request completes / cancels |
| 263 | Receivers MAY skip notifications / set frequency / omit total | MAY | ✅ | `()` client handler (`runtime.rs:1066,1079`) means rmcp routes incoming `ProgressNotification` to the blanket `ClientHandler::on_progress` default-impl which discards (SDK `handler/client.rs:201-203, 321-326`); we "skip" by virtue of having no handler |
| 264 | For task-augmented requests, the `progressToken` from the original request MUST continue to be used for progress notifications throughout task lifetime — valid until the task reaches a terminal status, even after `CreateTaskResult` returns | MUST | N/A | we do not implement task augmentation (Section 1 row 55 ❌); spec item only applies if we adopt `tasks` capability |
| 265 | Progress notifications for tasks MUST use the original `progressToken` | MUST | N/A | as above |
| 266 | Progress notifications for tasks MUST stop after terminal status | MUST | N/A | as above |
| 267 | Senders and receivers SHOULD track active progress tokens | SHOULD | N/A (vacuous) | we send no progress (row 255), we discard incoming progress (row 263) — nothing to track |
| 268 | Both parties SHOULD implement rate limiting on progress notifications | SHOULD | N/A (vacuous) | as above; rmcp does no rate limiting either way |
| 269 | Progress notifications MUST stop after completion | MUST | N/A (vacuous) | as above; spec compliance is the sender's responsibility |

### Improvement Plan (Jelly draft, pending Mira retroactive review)

- [ ] **Row 255 (emit `progressToken` on outbound requests)**: optionally set `_meta.progressToken` on `CallToolRequestParams` at `meta_tool.rs:95-98` so servers running long operations (file scans, large API queries, build steps) can stream progress back to us. Token would be a monotonically increasing per-server counter or a UUID; `()` client handler upgraded to a wrapper that routes `on_progress` to ACP `session/notify` so the orchestrator/user sees live updates.
  - **Eval**: rmcp covers most of the plumbing — `ProgressToken`, `ProgressNotificationParam`, `ClientHandler::on_progress` trait method all exist; `CallToolRequestParams` already supports `_meta` (verify: `with_meta` builder or direct field) · wrapper-sized in openab-agent (~150-200 LOC: token allocator, on_progress→ACP notify bridge, token-to-request-id mapping for row 262 compliance) · **fit: in-scope but borderline architectural**. Requires us to leave `()` handler — same architectural threshold as Section 1 rows 52-56 client capabilities. Worth bundling with `roots` capability work. Until then, document as known gap.
- [ ] **Row 268 (rate limiting on receive side)**: if we adopt outbound `progressToken` (row 255), add a simple `tokio::sync::Semaphore` or "max 10 notifications/sec per token" filter in the `on_progress` handler to protect the ACP/UI surface from chatty servers.
  - **Eval**: openab-agent only · drop-in (~30 LOC throttled stream) · **fit: in-scope as defensive layer**, only meaningful once row 255 lands. Score: defer until row 255.
- [ ] **Documentation**: in the same `openab-agent/docs/` matrix as Section 3 / Section 0 docs improvements, note that we do not currently emit or surface progress, and what users should expect for long-running tools.
  - **Eval**: docs only · drop-in · **fit: in-scope**. Cheap; honest about UX gap.

## Ping

Source: [`basic/utilities/ping.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/ping.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 270 | `ping` request method | (method) | | |
| 271 | Receiver MUST respond promptly with empty `{}` result | MUST | | |
| 272 | If no response within timeout, sender MAY consider connection stale / terminate / reconnect | MAY | | |
| 273 | Implementations SHOULD periodically issue pings to detect connection health | SHOULD | | |
| 274 | Ping frequency SHOULD be configurable | SHOULD | | |
| 275 | Ping timeouts SHOULD be appropriate for network environment | SHOULD | | |
| 276 | Excessive pinging SHOULD be avoided | SHOULD | | |
| 277 | Ping timeouts SHOULD be treated as connection failures | SHOULD | | |
| 278 | Multiple failed pings MAY trigger connection reset | MAY | | |
| 279 | Implementations SHOULD log ping failures for diagnostics | SHOULD | | |

## Tasks — experimental

Source: [`basic/utilities/tasks.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/tasks.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 280 | Servers and clients supporting tasks MUST declare `tasks` capability during init | MUST | | |
| 281 | Server `tasks` capability includes `list`, `cancel`, `requests.tools.call` | (capability) | | |
| 282 | Client `tasks` capability includes `list`, `cancel`, `requests.sampling.createMessage`, `requests.elicitation.create` | (capability) | | |
| 282a | `capabilities.tasks.requests` is exhaustive — request types not listed do NOT support task-augmentation | (capability) | | |
| 283 | Requestors SHOULD only augment requests with task if receiver declared the capability | SHOULD | | |
| 284 | If `capabilities.tasks` not defined, peer SHOULD NOT attempt to create tasks | SHOULD NOT | | |
| 285 | Tool-level: if server lacks `tasks.requests.tools.call`, clients MUST NOT attempt task augmentation regardless of `taskSupport` | MUST NOT | | |
| 286 | Tool-level: if `execution.taskSupport` absent or `"forbidden"`, clients MUST NOT invoke as task | MUST NOT | | |
| 287 | Tool-level: server SHOULD return `-32601` if client attempts task-augmentation on forbidden tool | SHOULD | | |
| 288 | Tool-level: if `execution.taskSupport == "optional"`, clients MAY invoke as task or normal | MAY | | |
| 289 | Tool-level: if `execution.taskSupport == "required"`, clients MUST invoke as task | MUST | | |
| 290 | Tool-level: server MUST return `-32601` if `taskSupport == "required"` and client does not use task | MUST | | |
| 291 | Requestors MAY include `ttl` value (ms) in task augmentation | MAY | | |
| 292 | Receivers accepting task-augmented request MUST return `CreateTaskResult` | MUST | | |
| 293 | `CreateTaskResult` SHOULD be returned as soon as possible after accepting | SHOULD | | |
| 293a | `CreateTaskResult` `_meta` MAY include `io.modelcontextprotocol/model-immediate-response` (string) suggesting an immediate-return value for the LLM while the task executes (provisional, non-binding) | MAY | | |
| 294 | Requestors SHOULD respect `pollInterval` in responses for polling frequency | SHOULD | | |
| 295 | Requestors SHOULD continue polling until terminal status or `input_required` | SHOULD | | |
| 296 | Even after invoking `tasks/result`, requestors SHOULD continue polling via `tasks/get` unless actively blocked waiting on the `tasks/result` response | SHOULD | | |
| 297 | Receivers MAY send `notifications/tasks/status` on status change | MAY | | |
| 298 | `notifications/tasks/status` includes full `Task` object | (notification) | | |
| 299 | Requestors MUST NOT rely on receiving `notifications/tasks/status` (it is optional) | MUST NOT | | |
| 300 | When sent, `notifications/tasks/status` SHOULD NOT include `io.modelcontextprotocol/related-task` metadata | SHOULD NOT | | |
| 301 | `tasks/list` operation supports pagination | (method) | | |
| 302 | Receivers MUST reject `tasks/cancel` on already-terminal tasks with `-32602` | MUST | | |
| 303 | Upon valid cancellation, receivers SHOULD attempt to stop execution and MUST transition to `cancelled` before responding | SHOULD/MUST | | |
| 304 | Once cancelled, task MUST remain in `cancelled` even if execution completes | MUST | | |
| 305 | Receivers MAY delete cancelled tasks at any time | MAY | | |
| 306 | Requestors SHOULD NOT rely on cancelled tasks being retained | SHOULD NOT | | |
| 307 | Receivers without task capability for a request type MUST process normally, ignoring task metadata | MUST | | |
| 308 | Receivers with task capability for a request type MAY return an error for non-task-augmented requests, effectively requiring task augmentation | MAY | | |
| 309 | Task IDs MUST be string values | MUST | | |
| 310 | Task IDs MUST be generated by the receiver | MUST | | |
| 311 | Task IDs MUST be unique among all tasks controlled by the receiver | MUST | | |
| 312 | Tasks MUST begin in `working` status | MUST | | |
| 313 | Receivers MUST only transition through valid paths: from `working` → `input_required`/`completed`/`failed`/`cancelled`; from `input_required` → `working`/`completed`/`failed`/`cancelled` | MUST | | |
| 314 | Terminal tasks (`completed`/`failed`/`cancelled`) MUST NOT transition to any other status | MUST NOT | | |
| 314a | For task-augmented `tools/call`, if the underlying tool result has `isError: true`, the task should reach `failed` status | SHOULD | | |
| 315 | When task needs requestor input, receiver SHOULD move task to `input_required` | SHOULD | | |
| 316 | When in `input_required`, receiver MUST include `io.modelcontextprotocol/related-task` metadata in any request it sends back to the requestor (e.g., the elicitation/sampling that the task depends on) | MUST | | |
| 317 | When requestor encounters `input_required`, it SHOULD preemptively call `tasks/result` | SHOULD | | |
| 318 | When receiver receives required input, task SHOULD transition out of `input_required` | SHOULD | | |
| 319 | Receivers MUST include `createdAt` ISO 8601 timestamp in all task responses | MUST | | |
| 320 | Receivers MUST include `lastUpdatedAt` ISO 8601 timestamp in all task responses | MUST | | |
| 321 | Receivers MAY override requested `ttl` | MAY | | |
| 322 | Receivers MUST include actual `ttl` (or `null` for unlimited) in `tasks/get` responses | MUST | | |
| 323 | After `ttl` elapsed, receivers MAY delete task and results | MAY | | |
| 324 | Receivers MAY include `pollInterval` (ms) in `tasks/get` responses | MAY | | |
| 325 | On `tasks/result` for terminal task, receiver MUST return the underlying request's final result/error | MUST | | |
| 326 | On `tasks/result` for non-terminal task, receiver MUST block the response until task reaches terminal status | MUST | | |
| 327 | For terminal tasks, `tasks/result` MUST return exactly what the original request would | MUST | | |
| 328 | All requests/notifications/responses related to a task MUST include `io.modelcontextprotocol/related-task` metadata | MUST | | |
| 329 | For `tasks/get`/`tasks/result`/`tasks/cancel`, `taskId` param MUST be source of truth | MUST | | |
| 330 | Requestors SHOULD NOT include `io.modelcontextprotocol/related-task` metadata in `tasks/get`/`tasks/result`/`tasks/cancel` request params (the `taskId` RPC param is source of truth) | SHOULD NOT | | |
| 330a | Receivers SHOULD NOT include related-task metadata in result messages for `tasks/get`/`tasks/list`/`tasks/cancel` (taskId already in response) | SHOULD NOT | | |
| 331 | Receivers MUST ignore related-task metadata if present in `tasks/get`/`tasks/result`/`tasks/cancel` requests, treating `taskId` RPC param as source of truth | MUST | | |
| 332 | `tasks/result` response MUST include the related-task metadata | MUST | | |
| 333 | Receivers SHOULD use cursor-based pagination for `tasks/list` | SHOULD | | |
| 334 | Receivers MUST include `nextCursor` if more tasks available | MUST | | |
| 335 | Requestors MUST treat cursors as opaque tokens | MUST | | |
| 336 | If task retrievable via `tasks/get`, it MUST be retrievable via `tasks/list` for same requestor | MUST | | |
| 337 | If `tasks/result` underlying request resulted in JSON-RPC error, `tasks/result` MUST return same error | MUST | | |
| 338 | If `tasks/result` underlying request returned response, `tasks/result` MUST return that response | MUST | | |
| 339 | Receivers MUST return `-32602` for invalid/nonexistent `taskId` in get/result/cancel | MUST | | |
| 340 | Receivers MUST return `-32602` for invalid/nonexistent cursor in `tasks/list` | MUST | | |
| 341 | Receivers MUST return `-32602` for cancellation of terminal task | MUST | | |
| 342 | Receivers MUST return `-32603` for internal errors | MUST | | |
| 343 | Receivers MAY return `-32600` if task augmentation required but not provided | MAY | | |
| 344 | Receivers SHOULD provide informative error messages | SHOULD | | |
| 345 | `tasks/get` response on failure SHOULD include diagnostic `statusMessage` | SHOULD | | |
| 346 | When auth context available, receivers MUST bind tasks to that context | MUST | | |
| 347 | If context-binding unavailable, receivers SHOULD document the limitation | SHOULD | | |
| 348 | If context-binding unavailable, receivers MUST generate cryptographically secure task IDs | MUST | | |
| 349 | Receivers unable to identify requestors SHOULD NOT declare `tasks.list` capability | SHOULD NOT | | |
| 350 | With context binding, receivers MUST reject cross-context `tasks/get`/`tasks/result`/`tasks/cancel` | MUST | | |
| 351 | With context binding, `tasks/list` results MUST include only tasks for requestor's context | MUST | | |
| 352 | Receivers SHOULD implement rate limiting on task operations | SHOULD | | |
| 353 | Receivers SHOULD enforce concurrent-task limits per requestor | SHOULD | | |
| 354 | Receivers SHOULD enforce maximum `ttl` to prevent indefinite retention | SHOULD | | |
| 355 | Receivers SHOULD clean up expired tasks promptly | SHOULD | | |
| 356 | Receivers SHOULD document max supported `ttl` and max concurrent tasks per requestor | SHOULD | | |
| 357 | Receivers SHOULD implement monitoring/alerting for resource usage | SHOULD | | |
| 358 | Receivers SHOULD log task creation/completion/retrieval events for audit | SHOULD | | |
| 359 | Receivers SHOULD include auth context in logs when available | SHOULD | | |
| 360 | Receivers SHOULD monitor for suspicious patterns | SHOULD | | |
| 361 | Requestors SHOULD log task lifecycle events for debugging/audit | SHOULD | | |
| 362 | Requestors SHOULD track task IDs and associated operations | SHOULD | | |
| 362a | On Streamable HTTP, clients MAY disconnect from an SSE stream opened in response to `tasks/get` or `tasks/result` at any time | MAY | | |
| 362b | Servers SHOULD NOT upgrade to an SSE stream in response to a `tasks/get` request | SHOULD NOT | | |
| 362c | Clients SHOULD expect task-related messages to be delivered on any SSE stream (including the HTTP GET stream) | SHOULD | | |

## Client / Roots

Source: [`client/roots.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/roots.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 363 | Clients supporting roots MUST declare `roots` capability | MUST | | |
| 364 | `roots.listChanged` sub-capability | (capability) | | |
| 365 | `roots/list` request method | (method) | | |
| 366 | `roots/list` result: `roots[]` of `{uri, name}` | (field) | | |
| 367 | Root `uri` MUST be `file://` URI | MUST | | |
| 368 | Root `name` is optional | (field) | | |
| 369 | On roots change, `listChanged`-capable clients MUST send `notifications/roots/list_changed` | MUST | | |
| 370 | Clients SHOULD return `-32601` (method not found) if roots unsupported, `-32603` for internal | SHOULD | | |
| 371 | Clients MUST only expose roots with appropriate permissions | MUST | | |
| 372 | Clients MUST validate all root URIs (path traversal) | MUST | | |
| 373 | Clients MUST implement proper access controls | MUST | | |
| 374 | Clients MUST monitor root accessibility | MUST | | |
| 375 | Servers SHOULD handle unavailable roots gracefully | SHOULD | | |
| 376 | Servers SHOULD respect root boundaries during operations | SHOULD | | |
| 377 | Servers SHOULD validate all paths against provided roots | SHOULD | | |
| 378 | Clients SHOULD prompt user consent before exposing roots | SHOULD | | |
| 379 | Clients SHOULD provide clear UI for root management | SHOULD | | |
| 380 | Clients SHOULD validate root accessibility before exposing | SHOULD | | |
| 381 | Clients SHOULD monitor for root changes | SHOULD | | |
| 382 | Servers SHOULD check for roots capability before usage | SHOULD | | |
| 383 | Servers SHOULD handle root list changes gracefully | SHOULD | | |
| 384 | Servers SHOULD cache root information appropriately | SHOULD | | |

## Client / Sampling

Source: [`client/sampling.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/sampling.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 385 | Clients supporting sampling MUST declare `sampling` capability | MUST | | |
| 386 | Clients supporting tool-enabled sampling MUST declare `sampling.tools` capability | MUST | | |
| 387 | Servers MUST NOT send tool-enabled sampling to clients without `sampling.tools` capability | MUST NOT | | |
| 387a | Client MUST return an error if `CreateMessageRequestParams.tools` is provided but client did not declare `ClientCapabilities.sampling.tools` (symmetric to row 387, per `schema.mdx` JSDoc) | MUST | | |
| 388 | `sampling.context` sub-capability (soft-deprecated) — servers SHOULD NOT use `includeContext` values `thisServer`/`allServers` unless client declares it | SHOULD NOT | | |
| 389 | Servers SHOULD avoid `includeContext` `thisServer`/`allServers` (soft-deprecated) | SHOULD | | |
| 390 | `sampling/createMessage` request | (method) | | |
| 391 | Request params: `messages`, `modelPreferences`, `systemPrompt`, `maxTokens`, `includeContext` (default `"none"`) | (field) | | |
| 391a | Client MAY ignore `modelPreferences` (per `schema.mdx` JSDoc) | MAY | | |
| 391b | Client MAY modify or omit `systemPrompt` (per `schema.mdx` JSDoc) | MAY | | |
| 391c | Client MAY ignore `includeContext` (per `schema.mdx` JSDoc) | MAY | | |
| 391d | Client MAY sample fewer tokens than `maxTokens` requested (per `schema.mdx` JSDoc) | MAY | | |
| 392 | Request params (tools): optional `tools[]`, `toolChoice` | (field) | | |
| 393 | Result fields: `role`, `content`, `model`, `stopReason` | (field) | | |
| 394 | Content types: text / image / audio / tool_use / tool_result | (field) | | |
| 394a | Client SHOULD preserve `ToolUseContent._meta` for caching optimizations (per `schema.mdx` JSDoc) | SHOULD | | |
| 394b | Client SHOULD preserve `ToolResultContent._meta` for caching optimizations (per `schema.mdx` JSDoc) | SHOULD | | |
| 395 | Tool-result user messages MUST contain ONLY tool results (no mixing) | MUST | | |
| 396 | Every assistant `ToolUseContent` block MUST be followed by user message of `ToolResultContent` matching by `toolUseId` | MUST | | |
| 397 | `toolChoice` modes: `auto`, `required`, `none` | (field) | | |
| 398 | `toolChoice: required` — model MUST use at least one tool before completing | MUST | | |
| 399 | `toolChoice: none` — model MUST NOT use any tools | MUST NOT | | |
| 400 | Model preferences: `costPriority`, `speedPriority`, `intelligencePriority` (0–1) | (field) | | |
| 401 | Model `hints[].name` substring-match | (field) | | |
| 401a | Client MUST evaluate `ModelPreferences.hints` in array order (per `schema.mdx` JSDoc) | MUST | | |
| 401b | Client SHOULD prioritize `hints` over numeric priorities; MAY use numeric priorities as fallback (per `schema.mdx` JSDoc) | SHOULD | | |
| 402 | Clients MAY map hints to equivalent models from different providers | MAY | | |
| 402a | Client MAY ignore `ModelHint.meta` (non-standard model-specific metadata, per `schema.mdx` JSDoc) | MAY | | |
| 403 | Human-in-the-loop SHOULD be able to deny sampling requests | SHOULD | | |
| 404 | Applications SHOULD provide UI to review requests, edit prompts, present responses | SHOULD | | |
| 405 | Clients SHOULD return errors for common failures (`-1` user rejected, `-32602` tool-result missing, `-32602` tool-results mixed) | SHOULD | | |
| 406 | Clients SHOULD implement user approval controls | SHOULD | | |
| 407 | Both parties SHOULD validate message content | SHOULD | | |
| 408 | Clients SHOULD respect model preference hints | SHOULD | | |
| 409 | Clients SHOULD implement rate limiting | SHOULD | | |
| 410 | Both parties MUST handle sensitive data appropriately | MUST | | |
| 411 | When replying to a `stopReason: "toolUse"` response, servers MUST respond to each `ToolUseContent` with a `ToolResultContent` of matching `toolUseId` | MUST | | |
| 412 | When tools are used, user message containing tool results MUST contain only tool results | MUST | | |
| 413 | Both parties SHOULD implement iteration limits for tool loops | SHOULD | | |

## Client / Elicitation

Source: [`client/elicitation.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/client/elicitation.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 414 | Clients supporting elicitation MUST declare `elicitation` capability | MUST | | |
| 415 | Empty `elicitation: {}` is equivalent to declaring `form` mode only | (compat) | | |
| 416 | Clients declaring `elicitation` MUST support at least one mode (`form` or `url`) | MUST | | |
| 417 | Servers MUST NOT send elicitation requests with modes not supported by the client | MUST NOT | | |
| 418 | All elicitation requests MUST include `message`; `mode` is required for URL mode and optional (defaults to `"form"`) for form mode | MUST | | |
| 419 | For backwards compat, servers MAY omit `mode` for form mode requests | MAY | | |
| 420 | Clients MUST treat absent `mode` as form mode | MUST | | |
| 421 | Form mode elicitation MUST either specify `mode:"form"` or omit `mode`, and include `requestedSchema` | MUST | | |
| 422 | `requestedSchema` is restricted: flat object, primitive types (string, number/integer, boolean, enum) | (constraint) | | |
| 422a | `requestedSchema` also supports multi-select enum: `type: "array"` with `items.enum` or `items.anyOf` (fifth schema kind beyond the primitives in row 422) | (constraint) | | |
| 423 | Supported string formats: `email`, `uri`, `date`, `date-time` | (constraint) | | |
| 424 | All primitive types support optional default values | (field) | | |
| 425 | Clients supporting defaults SHOULD pre-populate form fields with default values | SHOULD | | |
| 426 | URL mode elicitation MUST specify `mode:"url"`, `message`, `url`, `elicitationId` | MUST | | |
| 427 | `url` parameter MUST contain a valid URL | MUST | | |
| 428 | Servers MAY send `notifications/elicitation/complete` on URL-mode completion | MAY | | |
| 429 | Servers MUST only send completion notification to the client that initiated the elicitation | MUST | | |
| 430 | Completion notification MUST include the original `elicitationId` | MUST | | |
| 430a | Client MUST treat `ElicitRequestURLParams.elicitationId` as opaque (per `schema.mdx` JSDoc) | MUST | | |
| 431 | Clients MUST ignore completion notifications for unknown / already-completed IDs | MUST | | |
| 432 | Clients MAY wait for completion notification to retry / update UI / continue | MAY | | |
| 433 | Clients SHOULD still provide manual retry/cancel controls if notification never arrives | SHOULD | | |
| 434 | Servers MAY return `URLElicitationRequiredError` (-32042) | MAY | | |
| 435 | Server MUST NOT return `URLElicitationRequiredError` except when URL elicitation required | MUST NOT | | |
| 436 | The error MUST include list of required elicitations | MUST | | |
| 437 | Elicitations in error MUST be URL mode and have `elicitationId` | MUST | | |
| 438 | Servers MUST return `-32042` when request blocked on URL elicitation | MUST | | |
| 439 | Clients MUST return `-32602` when elicitation mode not declared in capabilities | MUST | | |
| 440 | Servers MUST NOT request sensitive info via form mode (passwords, API keys, access tokens, payment credentials) | MUST NOT | | |
| 441 | Servers MUST use URL mode for sensitive info interactions | MUST | | |
| 442 | Clients MUST provide UI making it clear which server is requesting | MUST | | |
| 443 | Clients MUST respect privacy with clear decline/cancel options | MUST | | |
| 444 | For form mode, clients MUST allow user review/modify before sending | MUST | | |
| 445 | For URL mode, clients MUST clearly display target domain/host and gather user consent before navigation | MUST | | |
| 446 | Three-action response model: accept / decline / cancel | (field) | | |
| 447 | Servers MUST bind elicitation requests to client and user identity | MUST | | |
| 448 | Servers implementing elicitation MUST securely associate user state per security best practices | MUST | | |
| 449 | State MUST NOT be associated with session IDs alone | MUST NOT | | |
| 450 | State storage MUST be protected against unauthorized access | MUST | | |
| 451 | Remote servers MUST derive user identification from MCP authorization credentials when possible | MUST | | |
| 452 | MCP servers MUST NOT rely on URL elicitation to authorize users for themselves | MUST NOT | | |
| 453 | Third-party credentials MUST NOT transit through the MCP client | MUST NOT | | |
| 454 | MCP server MUST NOT use client's MCP credentials for third-party service (no token passthrough) | MUST NOT | | |
| 455 | User MUST authorize MCP server directly for external authorization | MUST | | |
| 456 | MCP server MUST NOT transmit credentials obtained via URL elicitation to MCP client | MUST NOT | | |
| 457 | Servers MUST NOT include sensitive info / PII / credentials in elicitation URL | MUST NOT | | |
| 458 | Servers MUST NOT provide pre-authenticated URLs (impersonation risk) | MUST NOT | | |
| 459 | Servers SHOULD NOT include clickable URLs in form-mode fields | SHOULD NOT | | |
| 460 | Servers SHOULD use HTTPS URLs for non-development environments | SHOULD | | |
| 461 | Clients implementing URL mode MUST handle URLs carefully (prevent malicious links) | MUST | | |
| 462 | Clients MUST NOT auto-prefetch elicitation URLs or metadata | MUST NOT | | |
| 463 | Clients MUST NOT open URL without explicit user consent | MUST NOT | | |
| 464 | Clients MUST show full URL for user examination before consent | MUST | | |
| 465 | Clients MUST open URL in secure manner (no LLM/client inspection of content) | MUST | | |
| 466 | Clients SHOULD highlight URL domain to mitigate subdomain spoofing | SHOULD | | |
| 467 | Clients SHOULD warn on ambiguous/suspicious URIs (Punycode) | SHOULD | | |
| 468 | Clients SHOULD NOT render URLs as clickable in elicitation fields except the URL-mode `url` field | SHOULD NOT | | |
| 469 | Servers MUST NOT rely on client-provided user ID without server verification | MUST NOT | | |
| 470 | Servers SHOULD follow security best practices for user identification | SHOULD | | |
| 471 | Clients SHOULD validate all form responses against provided schema | SHOULD | | |
| 472 | Servers SHOULD validate received data matches requested schema | SHOULD | | |
| 473 | Servers MUST verify identity of user opening URL before accepting info (anti-phishing) | MUST | | |
| 474 | Server MUST ensure user who started elicitation is same user who completes authorization flow | MUST | | |
| 475 | Mechanism to determine user identity MUST be resilient to attacks where an attacker can modify the elicitation URL | MUST | | |
| 476 | Clients SHOULD implement user approval controls | SHOULD | | |
| 477 | Clients SHOULD allow users to decline elicitation requests at any time | SHOULD | | |
| 478 | Clients SHOULD implement rate limiting | SHOULD | | |
| 479 | Clients SHOULD present elicitation requests clearly (what / why) | SHOULD | | |

## Server / Tools

Source: [`server/tools.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 480 | Servers with tools MUST declare `tools` capability | MUST | | |
| 481 | `tools.listChanged` sub-capability bit | (capability) | | |
| 482 | `tools/list` with pagination | (method) | | |
| 483 | `tools/call` with `name` + `arguments` | (method) | | |
| 484 | Tool fields: name / title / description / inputSchema / outputSchema / annotations / icons / execution | (field) | | |
| 485 | `inputSchema` MUST be a valid JSON Schema object (not null) | MUST | | |
| 486 | `inputSchema` follows JSON Schema usage guidelines (default 2020-12) | (constraint) | | |
| 486a | For tools with no parameters, `inputSchema` SHOULD be `{"type":"object","additionalProperties":false}` (recommended) or `{"type":"object"}` | SHOULD | | |
| 487 | `outputSchema` follows JSON Schema usage guidelines (default 2020-12) | (constraint) | | |
| 488 | Tool names SHOULD be 1–128 characters (inclusive) | SHOULD | | |
| 488a | Tool names SHOULD be considered case-sensitive | SHOULD | | |
| 489 | Tool names SHOULD only contain A-Z, a-z, 0-9, `_`, `-`, `.` | SHOULD | | |
| 490 | Tool names SHOULD NOT contain spaces / commas / special chars | SHOULD NOT | | |
| 491 | Tool names SHOULD be unique within a server | SHOULD | | |
| 492 | `execution.taskSupport` values: `"forbidden"` (default), `"optional"`, `"required"` | (field) | | |
| 493 | Tool result content types: text / image / audio / resource_link / resource (embedded) | (field) | | |
| 494 | Content types support optional annotations (audience / priority / lastModified) | (field) | | |
| 495 | Tool MAY return `resource_link` items | MAY | | |
| 496 | Tool result MAY embed `resource` items | MAY | | |
| 497 | Servers using embedded resources SHOULD implement `resources` capability | SHOULD | | |
| 498 | Result: `content[]`, `isError`, optional `structuredContent` | (field) | | |
| 499 | Tools returning structured content SHOULD also return serialized JSON in a `TextContent` block (for backwards compatibility) | SHOULD | | |
| 500 | If `outputSchema` provided, servers MUST provide structured results matching | MUST | | |
| 501 | If `outputSchema` provided, clients SHOULD validate structured results against it | SHOULD | | |
| 502 | List-changed-capable servers SHOULD send `notifications/tools/list_changed` | SHOULD | | |
| 503 | `notifications/tools/list_changed` notification | (notification) | | |
| 504 | Two error mechanisms: protocol errors (JSON-RPC) + tool execution errors (`isError: true`) | (model) | | |
| 504a | Errors originating from tool execution SHOULD be reported inside `CallToolResult` (with `isError: true`), not as JSON-RPC protocol errors (per `schema.mdx` JSDoc) | SHOULD | | |
| 505 | Input validation errors are classified as tool execution errors (`isError: true`), not protocol errors | (classification) | | |
| 506 | Clients SHOULD provide tool execution errors to LLMs for self-correction | SHOULD | | |
| 507 | Clients MAY provide protocol errors to LLMs | MAY | | |
| 508 | Clients MUST consider tool annotations untrusted unless from trusted server | MUST | | |
| 509 | Human-in-the-loop SHOULD be able to deny tool invocations | SHOULD | | |
| 510 | Apps SHOULD show exposed tools + visual indicators + confirmation prompts | SHOULD | | |
| 511 | Servers MUST validate all tool inputs | MUST | | |
| 512 | Servers MUST implement proper access controls | MUST | | |
| 513 | Servers MUST rate-limit tool invocations | MUST | | |
| 514 | Servers MUST sanitize tool outputs | MUST | | |
| 515 | Clients SHOULD prompt for confirmation on sensitive operations | SHOULD | | |
| 516 | Clients SHOULD show tool inputs to user before calling server | SHOULD | | |
| 517 | Clients SHOULD validate tool results before passing to LLM | SHOULD | | |
| 518 | Clients SHOULD implement timeouts for tool calls | SHOULD | | |
| 519 | Clients SHOULD log tool usage for audit | SHOULD | | |

## Server / Prompts

Source: [`server/prompts.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/prompts.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 520 | Servers with prompts MUST declare `prompts` capability | MUST | | |
| 521 | `prompts.listChanged` sub-capability | (capability) | | |
| 522 | `prompts/list` with pagination | (method) | | |
| 523 | Result: `prompts[]`, optional `nextCursor` | (field) | | |
| 524 | `prompts/get` request | (method) | | |
| 525 | Result: `messages[]` (required), optional `description` | (field) | | |
| 526 | List-changed-capable servers SHOULD send `notifications/prompts/list_changed` | SHOULD | | |
| 527 | `notifications/prompts/list_changed` notification | (notification) | | |
| 528 | Prompt fields: `name` (required) / `title` / `description` / `arguments` / `icons` (all optional) | (field) | | |
| 529 | PromptMessage fields: role (user/assistant), content | (field) | | |
| 530 | Prompt content types: text / image / audio / resource | (field) | | |
| 531 | Image content MUST be base64-encoded with valid MIME | MUST | | |
| 532 | Audio content MUST be base64-encoded with valid MIME | MUST | | |
| 533 | Embedded resource MUST include valid URI, appropriate MIME, and text or blob | MUST | | |
| 534 | Servers SHOULD return `-32602` invalid prompt / missing args, `-32603` internal errors | SHOULD | | |
| 535 | Servers SHOULD validate prompt arguments before processing | SHOULD | | |
| 536 | Clients SHOULD handle pagination for large prompt lists | SHOULD | | |
| 537 | Both parties SHOULD respect capability negotiation | SHOULD | | |
| 538 | Implementations MUST validate prompt inputs/outputs to prevent injection | MUST | | |

## Server / Resources

Source: [`server/resources.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/resources.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 539 | Servers with resources MUST declare `resources` capability | MUST | | |
| 540 | `resources.subscribe` sub-capability (optional) | (capability) | | |
| 541 | `resources.listChanged` sub-capability (optional) | (capability) | | |
| 542 | `resources/list` request with pagination | (method) | | |
| 543 | Result: `resources[]`, optional `nextCursor` | (field) | | |
| 544 | `resources/read` with `uri` param | (method) | | |
| 545 | Result: `contents[]` | (field) | | |
| 546 | `resources/templates/list` request | (method) | | |
| 547 | Result: `resourceTemplates[]` | (field) | | |
| 547a | ResourceTemplate fields: `uriTemplate` (required, RFC 6570) / `name` (required) / `title` / `description` / `mimeType` / `icons` (optional) | (field) | | |
| 548 | List-changed-capable servers SHOULD send `notifications/resources/list_changed` | SHOULD | | |
| 549 | `resources/subscribe` request | (method) | | |
| 550 | `notifications/resources/updated` notification | (notification) | | |
| 551 | Resource fields: uri / name / title / description / mimeType / size / icons | (field) | | |
| 552 | Resource contents: text or base64 blob | (field) | | |
| 553 | Annotations (`audience` / `priority` / `lastModified`) apply to resources, resource templates, and content blocks | (field) | | |
| 554 | Servers SHOULD use `https://` only when client can fetch directly | SHOULD | | |
| 555 | Servers SHOULD prefer another URI scheme (built-in or custom) when not directly web-fetchable | SHOULD | | |
| 556 | MCP servers MAY use XDG MIME types (e.g. `inode/directory`) to identify non-regular `file://` resources without a standard MIME type | MAY | | |
| 556a | Standard URI schemes in spec: `https://`, `file://`, `git://` (servers MAY use custom schemes too) | (field) | | |
| 557 | Custom URI schemes MUST conform to RFC 3986 | MUST | | |
| 558 | Servers SHOULD return `-32002` resource-not-found, `-32603` internal | SHOULD | | |
| 559 | Servers MUST validate resource URIs | MUST | | |
| 560 | Access controls SHOULD be implemented for sensitive resources | SHOULD | | |
| 561 | Binary data MUST be properly encoded | MUST | | |
| 562 | Resource permissions SHOULD be checked before operations | SHOULD | | |

## Completion

Source: [`server/utilities/completion.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/completion.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 563 | Servers supporting completions MUST declare `completions` capability | MUST | | |
| 564 | `completion/complete` request | (method) | | |
| 565 | Params: `ref`, `argument`, optional `context.arguments` | (field) | | |
| 565a | Clients SHOULD include previously-resolved arguments in `context.arguments` for multi-argument refs | SHOULD | | |
| 566 | Reference types: `ref/prompt`, `ref/resource` | (field) | | |
| 567 | Result: `completion.values`, optional `total`, `hasMore` | (field) | | |
| 568 | Max 100 items per completion response | (constraint) | | |
| 569 | Servers SHOULD return `-32601` (capability not supported), `-32602` (invalid prompt name / missing required args), `-32603` (internal error) | SHOULD | | |
| 570 | Servers SHOULD return suggestions sorted by relevance | SHOULD | | |
| 571 | Servers SHOULD implement fuzzy matching where appropriate | SHOULD | | |
| 572 | Servers SHOULD rate-limit completion requests | SHOULD | | |
| 573 | Servers SHOULD validate all inputs | SHOULD | | |
| 574 | Clients SHOULD debounce rapid completion requests | SHOULD | | |
| 575 | Clients SHOULD cache completion results where appropriate | SHOULD | | |
| 576 | Clients SHOULD handle missing/partial results gracefully | SHOULD | | |
| 577 | Implementations MUST validate completion inputs | MUST | | |
| 578 | Implementations MUST implement appropriate rate limiting | MUST | | |
| 579 | Implementations MUST control access to sensitive suggestions | MUST | | |
| 580 | Implementations MUST prevent info disclosure via completions | MUST | | |

## Logging

Source: [`server/utilities/logging.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/logging.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 581 | Servers emitting log notifications MUST declare `logging` capability | MUST | | |
| 582 | Log levels follow RFC 5424 (debug, info, notice, warning, error, critical, alert, emergency) | (field) | | |
| 583 | `logging/setLevel` request | (method) | | |
| 584 | Clients MAY send `logging/setLevel` | MAY | | |
| 584a | Server MAY automatically decide log level if no `logging/setLevel` request has been received from the client (per `schema.mdx` JSDoc on `LoggingMessageParams`) | MAY | | |
| 585 | `notifications/message` with level / logger / data | (notification) | | |
| 586 | Servers SHOULD return `-32602` invalid level, `-32603` internal errors | SHOULD | | |
| 587 | Servers SHOULD rate-limit log messages | SHOULD | | |
| 588 | Servers SHOULD include context in `data` field | SHOULD | | |
| 589 | Servers SHOULD use consistent logger names | SHOULD | | |
| 590 | Servers SHOULD remove sensitive info | SHOULD | | |
| 591 | Clients MAY present / filter / persist log messages | MAY | | |
| 592 | Log messages MUST NOT contain credentials/secrets | MUST NOT | | |
| 593 | Log messages MUST NOT contain PII | MUST NOT | | |
| 594 | Log messages MUST NOT contain internal details aiding attacks | MUST NOT | | |
| 595 | Implementations SHOULD rate-limit, validate data, control log access, monitor for sensitive content | SHOULD | | |

## Pagination

Source: [`server/utilities/pagination.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/utilities/pagination.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 596 | Cursor-based pagination model | (model) | | |
| 597 | Clients MUST NOT assume fixed page size | MUST NOT | | |
| 598 | Response includes optional `nextCursor` | (field) | | |
| 599 | Request includes optional `cursor` | (field) | | |
| 600 | Paginated ops: resources/list, resources/templates/list, prompts/list, tools/list | (method) | | |
| 601 | Servers SHOULD provide stable cursors | SHOULD | | |
| 602 | Servers SHOULD handle invalid cursors gracefully | SHOULD | | |
| 603 | Clients SHOULD treat missing `nextCursor` as end of results | SHOULD | | |
| 604 | Clients SHOULD support both paginated and non-paginated flows | SHOULD | | |
| 605 | Clients MUST treat cursors as opaque tokens | MUST | | |
| 605a | Clients MUST NOT make assumptions about cursor format | MUST NOT | | |
| 606 | Clients MUST NOT parse or modify cursors | MUST NOT | | |
| 607 | Clients MUST NOT persist cursors across sessions | MUST NOT | | |
| 608 | Invalid cursors SHOULD result in `-32602` Invalid params | SHOULD | | |

## Trust, Safety & Consent (Key Principles)

Source: [`index.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/index.mdx)

The "Security and Trust & Safety" section of the spec is meta-governance — lowercase prose "must/should" forms principles (not BCP 14 normative), and the "Implementation Guidelines" subsection has 5 explicit **SHOULD** items. The protocol cannot enforce these at wire level, but implementations are tracked here for completeness.

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 609 | Users must explicitly consent to and understand all data access and operations | (principle) | | |
| 610 | Users must retain control over what data is shared and what actions are taken | (principle) | | |
| 611 | Implementors should provide clear UIs for reviewing and authorizing activities | (principle) | | |
| 612 | Hosts must obtain explicit user consent before exposing user data to servers | (principle) | | |
| 613 | Hosts must not transmit resource data elsewhere without user consent | (principle) | | |
| 614 | User data should be protected with appropriate access controls | (principle) | | |
| 615 | Tools represent arbitrary code execution and must be treated with appropriate caution | (principle) | | |
| 616 | Descriptions of tool behavior (annotations) should be considered untrusted unless from a trusted server | (principle) | | see also row 508 |
| 617 | Hosts must obtain explicit user consent before invoking any tool | (principle) | | |
| 618 | Users should understand what each tool does before authorizing its use | (principle) | | |
| 619 | Users must explicitly approve any LLM sampling requests | (principle) | | |
| 620 | Users should control: whether sampling occurs at all, the actual prompt sent, what results the server can see | (principle) | | |
| 621 | Implementors SHOULD build robust consent and authorization flows | SHOULD | | |
| 622 | Implementors SHOULD provide clear documentation of security implications | SHOULD | | |
| 623 | Implementors SHOULD implement appropriate access controls and data protections | SHOULD | | |
| 624 | Implementors SHOULD follow security best practices in their integrations | SHOULD | | |
| 625 | Implementors SHOULD consider privacy implications in their feature designs | SHOULD | | |
| 626 | Host enforces security policies and consent requirements | (role) | | from `architecture/index.mdx` Core Components |
| 627 | Host handles user authorization decisions | (role) | | from `architecture/index.mdx` Core Components |
| 628 | Host controls client connection permissions and lifecycle | (role) | | from `architecture/index.mdx` Core Components |

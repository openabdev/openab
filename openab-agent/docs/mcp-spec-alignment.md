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
| 1 | All implementations MUST support base protocol and lifecycle management | MUST | | |
| 2 | Other components MAY be implemented per app needs | MAY | | |
| 3 | All messages MUST follow JSON-RPC 2.0 | MUST | | |
| 4 | Requests MUST include a string or integer ID | MUST | | |
| 5 | Request ID MUST NOT be `null` | MUST NOT | | |
| 6 | Request ID MUST NOT be previously used by the requestor within the same session | MUST NOT | | |
| 7 | Result responses MUST include the same ID as the request | MUST | | |
| 8 | Result responses MUST include a `result` field | MUST | | |
| 9 | The `result` MAY follow any JSON object structure | MAY | | |
| 10 | Error responses MUST include the same ID as the request (except when malformed) | MUST | | |
| 11 | Error responses MUST include an `error` field with `code` and `message` | MUST | | |
| 12 | Error codes MUST be integers | MUST | | |
| 13 | Notification receivers MUST NOT send a response | MUST NOT | | |
| 14 | Notifications MUST NOT include an ID | MUST NOT | | |
| 15 | HTTP-based transports SHOULD conform to Authorization spec | SHOULD | | |
| 16 | STDIO transports SHOULD NOT follow auth spec; retrieve credentials from environment | SHOULD NOT | | |
| 17 | Clients/servers MAY negotiate custom auth | MAY | | |
| 18 | Implementations MUST support JSON Schema 2020-12 for schemas without explicit `$schema` | MUST | | |
| 19 | Implementations MUST validate schemas according to declared/default dialect | MUST | | |
| 20 | Implementations MUST handle unsupported dialects gracefully (return error indicating unsupported) | MUST | | |
| 21 | Implementations SHOULD document which schema dialects they support | SHOULD | | |
| 22 | Schemas MUST be valid according to their declared or default dialect | MUST | | |
| 23 | Implementors are RECOMMENDED to use JSON Schema 2020-12 | RECOMMENDED | | |
| 24 | Implementations MUST NOT make assumptions about values at reserved `_meta` keys | MUST NOT | | |
| 25 | `_meta` prefix (if specified) MUST be dot-separated labels followed by `/`; each label MUST start with a letter and end with a letter or digit (interior chars MAY be letters, digits, or `-`) | MUST | | |
| 26 | `_meta` prefixes containing `modelcontextprotocol` or `mcp` as second label are reserved | (reserved) | | |
| 27 | `_meta` name MUST begin and end with alphanumeric | MUST | | |
| 28 | `_meta` name MAY contain `-`, `_`, `.`, alphanumerics | MAY | | |
| 29 | Implementations SHOULD use reverse DNS notation for `_meta` prefixes | SHOULD | | |
| 30 | Icon-rendering clients MUST support `image/png` and `image/jpeg` (and `image/jpg`) MIME types | MUST | | |
| 31 | Icon-rendering clients SHOULD also support `image/svg+xml` and `image/webp` | SHOULD | | |
| 32 | Icon consumers MUST take appropriate security precautions when handling icons | MUST | | |
| 33 | Clients MUST reject icon URIs with unsafe schemes (`javascript:`, `file:`, `ftp:`, `ws:`, local-app); MUST disallow scheme changes and cross-origin redirects | MUST | | |
| 34 | Icon consumers MAY set limits for image size, dimensions, frame count | MAY | | |
| 35 | Icons SHOULD be fetched without credentials — do not send cookies, `Authorization` headers, or client credentials | (security) | | |
| 36 | Icon consumers MAY disallow specific file types or sanitize before rendering | MAY | | |
| 37 | Validate MIME types and file contents before rendering icons (treat declared MIME as advisory; detect via magic bytes; reject on mismatch/unknown); maintain a strict allowlist of image types | (security) | | |
| 37a | Verify icon URIs originate from the same origin as the server (cross-origin icons require explicit handling) | (security) | | |
| 37b | JSON-RPC `Error.message` SHOULD be limited to a concise single sentence (per `schema.mdx` JSDoc) | SHOULD | | |

## Lifecycle

Source: [`basic/lifecycle.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/lifecycle.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 38 | Initialization phase MUST be first interaction | MUST | | |
| 39 | Client MUST initiate the `initialize` request | MUST | | |
| 40 | `initialize` request carries `protocolVersion`, `capabilities`, `clientInfo` | (field) | | |
| 41 | Server MUST respond with `protocolVersion`, `capabilities`, `serverInfo`; MAY include `instructions` | MUST | | |
| 42 | After successful initialization, client MUST send `notifications/initialized` | MUST | | |
| 43 | Client SHOULD NOT send other requests pre-init except ping | SHOULD NOT | | |
| 44 | Server SHOULD NOT send other requests pre-init except ping/logging | SHOULD NOT | | |
| 45 | Client MUST send a supported `protocolVersion` in `initialize` | MUST | | |
| 45a | Client MAY support older `protocolVersion` values for backwards compatibility (per `schema.mdx` JSDoc on `InitializeRequest.params.protocolVersion`) | MAY | | |
| 46 | Client SHOULD send the latest version it supports | SHOULD | | |
| 47 | If server supports the requested version, it MUST echo same version | MUST | | |
| 48 | Otherwise server MUST respond with another supported version | MUST | | |
| 49 | Server SHOULD respond with its latest supported version | SHOULD | | |
| 50 | If client does not support server's response version, client SHOULD disconnect | SHOULD | | |
| 50a | ⚠️ Spec internal conflict: `schema.mdx` (`InitializeResult.protocolVersion` JSDoc) states this as `MUST disconnect`. Alignment doc follows the prose source `basic/lifecycle.mdx` (`SHOULD`); upstream should reconcile. | (spec-conflict) | | |
| 51 | HTTP: client MUST include `MCP-Protocol-Version` header on subsequent requests | MUST | | |
| 52 | Client capability: `roots` (with optional `listChanged`) | (capability) | | |
| 53 | Client capability: `sampling` (LLM sampling support; `tools`/`context` sub-objects defined in `client/sampling.mdx`, not in `lifecycle.mdx` capability table) | (capability) | | |
| 54 | Client capability: `elicitation` (form/URL elicitation support; example shows `form`/`url` sub-objects but capability table lists only `elicitation`) | (capability) | | |
| 55 | Client capability: `tasks` with `requests.*` describing which incoming request types support task-augmentation (no `list`/`cancel` on client side — those are server-only) | (capability) | | |
| 56 | Client capability: `experimental` | (capability) | | |
| 57 | Server capability: `prompts` (with optional `listChanged`) | (capability) | | |
| 58 | Server capability: `resources` (with optional `subscribe`, `listChanged`) | (capability) | | |
| 59 | Server capability: `tools` (with optional `listChanged`) | (capability) | | |
| 60 | Server capability: `logging` | (capability) | | |
| 61 | Server capability: `completions` | (capability) | | |
| 62 | Server capability: `tasks` (with `list`, `cancel`, `requests.*`) | (capability) | | |
| 63 | Server capability: `experimental` | (capability) | | |
| 64 | Both parties MUST respect the negotiated protocol version | MUST | | |
| 65 | Both parties MUST only use successfully negotiated capabilities | MUST | | |
| 66 | stdio shutdown: client SHOULD close stdin, wait, SIGTERM, then SIGKILL | SHOULD | | |
| 67 | Server MAY initiate stdio shutdown by closing its output and exiting | MAY | | |
| 68 | HTTP shutdown by closing associated HTTP connection(s) | (transport) | | |
| 69 | Implementations SHOULD establish timeouts on all sent requests | SHOULD | | |
| 70 | On timeout, sender SHOULD issue a cancellation notification | SHOULD | | |
| 71 | SDKs/middleware SHOULD allow per-request timeout configuration | SHOULD | | |
| 72 | Implementations MAY reset timeout clock on receiving a progress notification | MAY | | |
| 73 | Implementations SHOULD always enforce a maximum timeout (even with progress) | SHOULD | | |
| 74 | Implementations SHOULD handle version mismatch, capability failures, timeouts | SHOULD | | |
| 74a | `Implementation` object (clientInfo / serverInfo) carries optional `title`, `description`, `icons`, `websiteUrl` fields | (schema) | | |

## Transports

Source: [`basic/transports.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 75 | JSON-RPC messages MUST be UTF-8 | MUST | | |
| 76 | Clients SHOULD support stdio whenever possible | SHOULD | | |
| 77 | stdio messages delimited by newlines; MUST NOT contain embedded newlines | MUST NOT | | |
| 78 | Server MAY write UTF-8 to stderr for any logging (including non-error) | MAY | | |
| 79 | Client MAY capture/forward/ignore server's stderr | MAY | | |
| 80 | Client SHOULD NOT assume stderr indicates errors | SHOULD NOT | | |
| 81 | Server MUST NOT write non-MCP to stdout | MUST NOT | | |
| 82 | Client MUST NOT write non-MCP to server's stdin | MUST NOT | | |
| 83 | Streamable HTTP server MUST provide single endpoint supporting POST + GET | MUST | | |
| 84 | Server MUST validate `Origin` header on all incoming connections (DNS rebinding defence) | MUST | | |
| 85 | If Origin header is present and invalid, server MUST respond with HTTP 403 Forbidden | MUST | | |
| 85a | The 403 Forbidden response body MAY comprise a JSON-RPC error response with no `id` | MAY | | |
| 86 | Local servers SHOULD bind only to localhost (127.0.0.1), not all network interfaces (0.0.0.0) | SHOULD | | |
| 87 | Servers SHOULD implement proper authentication on all connections | SHOULD | | |
| 88 | Every client JSON-RPC message MUST be a new HTTP POST | MUST | | |
| 89 | Client MUST use HTTP POST to send messages to the MCP endpoint | MUST | | |
| 90 | Client MUST include `Accept: application/json, text/event-stream` | MUST | | |
| 91 | POST body MUST be a single JSON-RPC request, notification, or response | MUST | | |
| 92 | Server MUST return HTTP 202 Accepted (no body) on accepted notification/response input | MUST | | |
| 93 | If notification/response input is rejected, server MUST return an HTTP error status (e.g., 400 Bad Request) | MUST | | |
| 94 | Error response body MAY be a JSON-RPC error response with no `id` | MAY | | |
| 95 | For JSON-RPC request input, server MUST return either `Content-Type: text/event-stream` (SSE stream) or `Content-Type: application/json` (single JSON object) | MUST | | |
| 96 | Client MUST support both SSE and JSON response content types | MUST | | |
| 97 | On SSE initiation, server SHOULD immediately send event with ID + empty `data` to prime reconnection | SHOULD | | |
| 98 | After event-ID-bearing SSE event, server MAY close connection (without terminating SSE stream) | MAY | | |
| 99 | Client SHOULD poll SSE stream by reconnecting when server closes connection | SHOULD | | |
| 100 | If server closes connection before terminating SSE stream, it SHOULD send a `retry` SSE field | SHOULD | | |
| 101 | Client MUST respect SSE `retry` field, waiting that many ms before reconnect | MUST | | |
| 102 | SSE stream SHOULD eventually include the JSON-RPC response for the originating request | SHOULD | | |
| 103 | Server MAY send other requests/notifications on SSE before the response | MAY | | |
| 104 | Pre-response messages SHOULD relate to originating request | SHOULD | | |
| 105 | Server MAY terminate SSE stream if session expires | MAY | | |
| 106 | After response sent, server SHOULD terminate SSE stream | SHOULD | | |
| 107 | Disconnection MAY occur at any time | MAY | | |
| 108 | Disconnection SHOULD NOT be interpreted as request cancellation | SHOULD NOT | | |
| 109 | To cancel, client SHOULD send `CancelledNotification` | SHOULD | | |
| 110 | Server MAY make stream resumable to avoid message loss on disconnect | MAY | | |
| 111 | Client MAY issue HTTP GET to open SSE listening stream | MAY | | |
| 112 | GET MUST include `Accept: text/event-stream` | MUST | | |
| 113 | On GET, server MUST return `Content-Type: text/event-stream` or HTTP 405 Method Not Allowed (indicating no SSE at this endpoint) | MUST | | |
| 114 | Server MAY send JSON-RPC requests/notifications on GET SSE stream | MAY | | |
| 115 | GET-stream messages SHOULD be unrelated to concurrent client requests | SHOULD | | |
| 116 | Server MUST NOT send a JSON-RPC response on GET stream unless resuming a previous request | MUST NOT | | |
| 117 | Server MAY close GET SSE stream at any time | MAY | | |
| 118 | If server closes GET connection without terminating stream, it SHOULD send `retry` (same polling behavior) | SHOULD | | |
| 119 | Client MAY close SSE stream at any time | MAY | | |
| 120 | Client MAY remain connected to multiple SSE streams simultaneously | MAY | | |
| 121 | Server MUST send each JSON-RPC message on only one stream (no broadcasting) | MUST | | |
| 122 | Servers MAY attach `id` to SSE events for resumability | MAY | | |
| 123 | If present, SSE event ID MUST be globally unique across all streams within the session (or across all streams for that client if session management is not in use) | MUST | | |
| 124 | Event IDs SHOULD encode sufficient info to identify the originating stream | SHOULD | | |
| 125 | To resume after disconnect, client SHOULD issue HTTP GET with `Last-Event-ID` header (regardless of original transport) | SHOULD | | |
| 126 | Server MAY replay messages from `Last-Event-ID` on the disconnected stream | MAY | | |
| 127 | Server MUST NOT replay messages from a different stream | MUST NOT | | |
| 128 | Server MAY assign session ID at initialization by including `MCP-Session-Id` header on the HTTP response containing the `InitializeResult` | MAY | | |
| 129 | Session ID SHOULD be globally unique and cryptographically secure | SHOULD | | |
| 130 | Session ID MUST only contain visible ASCII (0x21–0x7E) | MUST | | |
| 131 | Client MUST handle session ID securely | MUST | | |
| 132 | Client MUST include `MCP-Session-Id` on all subsequent HTTP requests when issued | MUST | | |
| 133 | Servers requiring a session SHOULD respond HTTP 400 to non-init requests without `MCP-Session-Id` | SHOULD | | |
| 134 | Server MAY terminate session at any time | MAY | | |
| 135 | Post-termination, server MUST respond HTTP 404 to requests with that session ID | MUST | | |
| 136 | On HTTP 404 with session ID, client MUST start a new session via fresh `InitializeRequest` (no session ID) | MUST | | |
| 137 | Client SHOULD send HTTP DELETE with `MCP-Session-Id` to terminate session | SHOULD | | |
| 138 | Server MAY return HTTP 405 to DELETE | MAY | | |
| 139 | Client MUST include `MCP-Protocol-Version: <protocol-version>` header on all HTTP requests | MUST | | |
| 140 | Sent protocol-version header value SHOULD be the negotiated one | SHOULD | | |
| 141 | If server receives no `MCP-Protocol-Version` header and has no other way to identify the version (e.g., via initialization negotiation), it SHOULD assume `2025-03-26` | SHOULD | | |
| 142 | If invalid/unsupported `MCP-Protocol-Version` is sent, server MUST respond HTTP 400 | MUST | | |
| 143 | Implementations MAY implement custom transports | MAY | | |
| 144 | Custom transports MUST preserve JSON-RPC + lifecycle | MUST | | |
| 145 | Custom transports SHOULD document connection establishment / message exchange patterns | SHOULD | | |
| 145a | Client MAY implement legacy HTTP+SSE backwards-compat flow: POST `InitializeRequest`; on HTTP 400/404/405 fall back to GET expecting `endpoint` SSE event (for interop with 2024-11-05 HTTP+SSE servers) | MAY | | |
| 145b | Servers wanting to support older clients SHOULD continue to host both the SSE and POST endpoints of the old transport, alongside the new MCP endpoint | SHOULD | | |

## Authorization

Source: [`basic/authorization.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/authorization.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 146 | Authorization is OPTIONAL | OPTIONAL | | |
| 147 | HTTP transports SHOULD conform to authorization spec | SHOULD | | |
| 148 | STDIO SHOULD NOT follow this spec; credentials from environment | SHOULD NOT | | |
| 149 | Alternative transports MUST follow established security best practices for their protocol | MUST | | |
| 150 | Authorization servers MUST implement OAuth 2.1 with appropriate security measures for both confidential and public clients | MUST | | |
| 151 | AS and MCP clients SHOULD support OAuth Client ID Metadata Documents | SHOULD | | |
| 152 | AS and MCP clients MAY support RFC 7591 Dynamic Client Registration | MAY | | |
| 153 | MCP servers MUST implement RFC 9728 Protected Resource Metadata | MUST | | |
| 154 | MCP clients MUST use RFC 9728 PRM for authorization server discovery | MUST | | |
| 155 | AS MUST provide at least one of: RFC 8414 AS metadata, OpenID Connect Discovery 1.0 | MUST | | |
| 156 | MCP clients MUST support both AS metadata discovery mechanisms (RFC 8414 + OIDC Discovery 1.0) | MUST | | |
| 157 | PRM document MUST include `authorization_servers` field with ≥1 AS | MUST | | |
| 158 | MCP servers MUST implement one of the following PRM discovery mechanisms: `resource_metadata` in `WWW-Authenticate` on 401, or RFC 9728 well-known URI | MUST | | |
| 159 | MCP clients MUST support both PRM discovery mechanisms (header + well-known fallback) | MUST | | |
| 160 | MCP servers SHOULD include `scope` in `WWW-Authenticate` (per RFC 6750 §3) | SHOULD | | |
| 161 | Clients MUST NOT assume relationship between `WWW-Authenticate` scope set and `scopes_supported` | MUST NOT | | |
| 162 | Clients MUST treat challenge-provided scopes as authoritative for current request | MUST | | |
| 163 | Servers SHOULD strive for consistency in scope set construction | SHOULD | | |
| 164 | MCP clients MUST be able to parse `WWW-Authenticate` headers and respond appropriately to 401 | MUST | | |
| 165 | If `scope` is absent from `WWW-Authenticate`, clients SHOULD apply Scope Selection Strategy fallback | SHOULD | | |
| 166 | Clients MUST attempt multiple well-known endpoints (RFC 8414 + OIDC) when discovering AS metadata | MUST | | |
| 167 | For path-bearing issuer URLs, clients MUST try priority order: oauth-authorization-server path-insert, openid-configuration path-insert, openid-configuration appended | MUST | | |
| 168 | For pathless issuer URLs, clients MUST try oauth-authorization-server, then openid-configuration | MUST | | |
| 169 | Clients supporting all registration options SHOULD prefer pre-registered, then CIMD, then DCR, then prompt | SHOULD | | |
| 170 | MCP clients and AS SHOULD support OAuth Client ID Metadata Documents | SHOULD | | |
| 171 | CIMD-supporting MCP implementations MUST follow OAuth CIMD requirements | MUST | | |
| 172 | CIMD: clients MUST host metadata document at HTTPS URL per RFC requirements | MUST | | |
| 173 | CIMD: `client_id` URL MUST use `https` scheme with a path component | MUST | | |
| 174 | CIMD: metadata MUST include at least `client_id`, `client_name`, `redirect_uris` | MUST | | |
| 175 | CIMD: clients MUST ensure `client_id` value matches the document URL exactly | MUST | | |
| 176 | CIMD: clients MAY use `private_key_jwt` for client authentication | MAY | | |
| 177 | CIMD: MCP clients SHOULD check for `client_id_metadata_document_supported` AS capability | SHOULD | | |
| 178 | CIMD: MCP clients MAY fall back to DCR or pre-registration if CIMD unavailable | MAY | | |
| 178a | CIMD (AS-side): AS SHOULD fetch metadata documents when encountering URL-formatted `client_id`s | SHOULD | N/A — client-side | |
| 178b | CIMD (AS-side): AS MUST validate fetched document's `client_id` matches the URL exactly | MUST | N/A — client-side | |
| 178c | CIMD (AS-side): AS SHOULD cache metadata respecting HTTP cache headers | SHOULD | N/A — client-side | |
| 178d | CIMD (AS-side): AS MUST validate redirect URIs in authorization request against metadata document | MUST | N/A — client-side | |
| 178e | CIMD (AS-side): AS MUST validate metadata document structure is valid JSON and contains required fields | MUST | N/A — client-side | |
| 179 | Pre-registration: MCP clients SHOULD support an option for static client credentials | SHOULD | | |
| 180 | MCP clients and AS MAY support RFC 7591 Dynamic Client Registration | MAY | | |
| 181 | Scope Selection: clients SHOULD follow least privilege when requesting scopes | SHOULD | | |
| 182 | Scope Selection: clients SHOULD prefer `scope` from initial `WWW-Authenticate` header, else `scopes_supported` from PRM, else omit `scope` | SHOULD | | |
| 183 | MCP clients MUST implement RFC 8707 Resource Indicators (`resource` parameter) | MUST | | |
| 184 | `resource` parameter MUST be included in both authorization and token requests | MUST | | |
| 185 | `resource` parameter MUST identify the intended MCP server | MUST | | |
| 186 | `resource` MUST use the canonical URI per RFC 8707 §2 | MUST | | |
| 187 | MCP clients SHOULD provide the most specific URI possible for the MCP server | SHOULD | | |
| 188 | Implementations SHOULD accept uppercase scheme/host for robustness | SHOULD | | |
| 189 | Implementations SHOULD consistently use no-trailing-slash form for interoperability | SHOULD | | |
| 190 | MCP clients MUST send `resource` regardless of AS support | MUST | | |
| 191 | Access token handling MUST conform to OAuth 2.1 §5 | MUST | | |
| 192 | MCP client MUST use `Authorization: Bearer <access-token>` header | MUST | | |
| 193 | Authorization MUST be included on every HTTP request from client to server | MUST | | |
| 194 | Access tokens MUST NOT be in URI query | MUST NOT | | |
| 195 | MCP clients MUST NOT send tokens to the MCP server other than ones issued by the MCP server's AS | MUST NOT | | |
| 196 | MCP servers MUST validate access tokens per OAuth 2.1 §5.2 | MUST | | |
| 197 | MCP servers MUST validate tokens were issued specifically for them (audience) | MUST | | |
| 198 | On validation failure, MCP servers MUST follow OAuth 2.1 §5.3 error handling | MUST | | |
| 199 | Invalid/expired tokens MUST receive HTTP 401 | MUST | | |
| 200 | MCP servers MUST only accept tokens valid for their own resources | MUST | | |
| 201 | MCP servers MUST NOT accept or transit any other tokens | MUST NOT | | |
| 202 | Servers MUST return appropriate HTTP status (401/403/400) for auth errors | MUST | | |
| 203 | On runtime insufficient scope, server SHOULD return 403 + `WWW-Authenticate` with `error="insufficient_scope"`, `scope`, `resource_metadata`, optional `error_description` | SHOULD | | |
| 204 | On insufficient-scope error, servers SHOULD include required scopes in `scope` parameter | SHOULD | | |
| 205 | Servers SHOULD be consistent in scope inclusion strategy | SHOULD | | |
| 206 | Servers SHOULD consider UX impact when choosing scopes for insufficient-scope errors | SHOULD | | |
| 207 | Clients SHOULD respond to scope errors via step-up authorization flow OR handle the errors in other appropriate ways | SHOULD | | |
| 207a | Clients acting on behalf of a user SHOULD attempt the step-up authorization flow | SHOULD | | |
| 208 | `client_credentials` clients MAY attempt step-up authorization or abort | MAY | | |
| 209 | Clients SHOULD implement retry limits and track scope-upgrade attempts | SHOULD | | |
| 210 | Implementations MUST follow OAuth 2.1 §7 Security Considerations | MUST | | |
| 211 | MCP clients MUST include `resource` parameter for audience binding | MUST | | |
| 212 | MCP servers MUST validate tokens are issued for their own use | MUST | | |
| 213 | Clients and servers MUST implement secure token storage per OAuth 2.1 §7.1 | MUST | | |
| 214 | AS SHOULD issue short-lived access tokens | SHOULD | | |
| 215 | For public clients, AS MUST rotate refresh tokens | MUST | | |
| 216 | Implementations MUST follow OAuth 2.1 §1.5 Communication Security | MUST | | |
| 217 | All AS endpoints MUST be HTTPS | MUST | | |
| 218 | All redirect URIs MUST be localhost or HTTPS | MUST | | |
| 219 | MCP clients MUST implement PKCE per OAuth 2.1 §7.5.2 | MUST | | |
| 220 | MCP clients MUST verify PKCE support before proceeding with authorization | MUST | | |
| 221 | MCP clients MUST use `S256` code challenge method when technically capable (OAuth 2.1 §4.1.1) | MUST | | |
| 222 | OAuth 2.0 AS metadata: if `code_challenge_methods_supported` absent, clients MUST refuse to proceed | MUST | | |
| 223 | OIDC Discovery 1.0: clients MUST verify `code_challenge_methods_supported` is present; refuse if absent | MUST | | |
| 224 | AS providing OIDC Discovery 1.0 MUST include `code_challenge_methods_supported` | MUST | | |
| 225 | MCP clients MUST have redirect URIs registered with the AS | MUST | | |
| 226 | AS MUST validate exact redirect URIs against pre-registered values | MUST | | |
| 227 | MCP clients SHOULD use and verify `state` parameter, discard mismatches | SHOULD | | |
| 228 | AS MUST take precautions to prevent redirecting to untrusted URIs | MUST | | |
| 229 | AS SHOULD only auto-redirect if URI is trusted | SHOULD | | |
| 230 | AS implementing CIMD MUST consider security implications per CIMD §6 | MUST | | |
| 231 | AS fetching CIMD documents SHOULD consider SSRF risks | SHOULD | | |
| 232 | AS SHOULD display additional warnings for `localhost`-only redirect URIs | SHOULD | | |
| 233 | AS MAY require additional attestation mechanisms for enhanced security (esp. in the context of `localhost` redirect URIs) | MAY | | |
| 234 | AS MUST clearly display the redirect URI hostname during authorization | MUST | | |
| 235 | AS MAY implement domain-based trust policies | MAY | | |
| 236 | MCP proxies with static client IDs MUST obtain user consent for each dynamically registered client | MUST | | |
| 237 | MCP servers MUST validate access tokens before processing requests | MUST | | |
| 238 | MCP servers MUST follow OAuth 2.1 §5.2 for token validation | MUST | | |
| 239 | MCP servers MUST only accept tokens specifically intended for themselves | MUST | | |
| 240 | MCP servers MUST reject tokens that do not include them in the audience claim, or otherwise verify they are the intended recipient | MUST | | |
| 241 | MCP servers MUST NOT pass through MCP-client tokens to upstream APIs | MUST NOT | | |
| 242 | MCP clients MUST implement and use the RFC 8707 `resource` parameter (aligns with RFC 9728 §7.4 recommendation) | MUST | | |

## Cancellation

Source: [`basic/utilities/cancellation.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/cancellation.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 243 | `notifications/cancelled` carries `requestId` and optional `reason` | (notification) | | |
| 244 | Cancellation notification MUST only reference requests issued in the same direction | MUST | | |
| 245 | Cancellation notification MUST only target requests believed in-progress | MUST | | |
| 246 | `initialize` request MUST NOT be cancelled by clients | MUST NOT | | |
| 247 | For task-augmented requests, the `tasks/cancel` request MUST be used instead of the `notifications/cancelled` notification (tasks have a dedicated cancellation that returns final state) | MUST | | |
| 248 | Receivers SHOULD stop processing, free resources, not respond | SHOULD | | |
| 249 | Receivers MAY ignore cancellation if request unknown / complete / uncancellable | MAY | | |
| 250 | Sender SHOULD ignore any late response to a cancelled request | SHOULD | | |
| 251 | Both parties MUST handle cancel race conditions gracefully | MUST | | |
| 252 | Both parties SHOULD log cancellation reasons | SHOULD | | |
| 253 | Application UIs SHOULD indicate cancellation state | SHOULD | | |
| 254 | Invalid cancellation notifications SHOULD be ignored | SHOULD | | |

## Progress

Source: [`basic/utilities/progress.mdx`](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/progress.mdx)

| # | Item | Normative | Status | Location / Notes |
|---|---|---|---|---|
| 255 | `progressToken` carried in request `_meta` | (field) | | |
| 256 | Progress tokens MUST be string or integer | MUST | | |
| 257 | Progress tokens MUST be unique across active requests | MUST | | |
| 258 | `notifications/progress` carries token, progress, optional total/message | (notification) | | |
| 259 | `progress` value MUST increase with each notification, even if total is unknown | MUST | | |
| 260 | `progress` and `total` MAY be float | MAY | | |
| 261 | `message` field SHOULD provide relevant human-readable progress information | SHOULD | | |
| 262 | Progress notifications MUST only reference active in-progress operation tokens | MUST | | |
| 263 | Receivers MAY skip notifications / set frequency / omit total | MAY | | |
| 264 | For task-augmented requests, the `progressToken` from the original request MUST continue to be used for progress notifications throughout task lifetime — valid until the task reaches a terminal status, even after `CreateTaskResult` returns | MUST | | |
| 265 | Progress notifications for tasks MUST use the original `progressToken` | MUST | | |
| 266 | Progress notifications for tasks MUST stop after terminal status | MUST | | |
| 267 | Senders and receivers SHOULD track active progress tokens | SHOULD | | |
| 268 | Both parties SHOULD implement rate limiting on progress notifications | SHOULD | | |
| 269 | Progress notifications MUST stop after completion | MUST | | |

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

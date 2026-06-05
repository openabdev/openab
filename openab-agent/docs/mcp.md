# MCP support in openab-agent

This document describes how `openab-agent` works as a **Model Context Protocol (MCP)
client**: what is implemented, what is deliberately not, and why. It is the
reader-facing companion to [`mcp-spec-alignment.md`](./mcp-spec-alignment.md) (the
exhaustive per-spec-item compliance table, audited against MCP **2025-11-25** on
SDK **rmcp 1.7.0**).

## What openab-agent is

`openab-agent` is a **headless meta-tool gateway**, not an interactive UI client. It
sits underneath an ACP host and exposes connected MCP servers' tools to an
LLM-driven dispatch loop. Two consequences run through every capability below:

- **No UI surface.** Anything in the spec that assumes a human-facing client
  (icon rendering, URL-mode elicitation consent screens, live progress bars) is
  N/A — there is nowhere to render it.
- **LLM-driven, blocking dispatch.** The LLM issues a tool call and blocks on the
  final result. There is no live-progress consumer, so intermediate streaming
  (progress notifications) has no destination.

It connects over **stdio** and **streamable HTTP** transports (SSE retry honoured
where rmcp supports it).

## Capability matrix (openab-agent)

| Capability | Status | Summary |
|---|---|---|
| Tools (list / call / describe) | ✅ | Full; enriched projection (title, annotations, schema, task-support). |
| JSON Schema dialect | ✅ passthrough | draft 2020-12 supported; foreign dialect surfaced advisory-only; no validator (by design). |
| `_meta` keys | ✅ opaque | Never read or rewritten; passed through untouched. |
| Icon rendering | N/A | No UI surface. |
| Authorization (OAuth) | ⚠️ | rmcp `AuthorizationManager`: PRM/discovery + PKCE-S256-check ✅; step-up detect-only ⚠️; `resource` hardcode caveat. |
| Sampling (`createMessage`) | ✅ text-only | Routed to the agent's LLM provider; env-var approval gate; no `sampling.tools`. |
| Roots | ✅ | Static set (cwd + config allow-list); no `listChanged`. |
| Elicitation | ⚠️ form-only | Form-mode via ACP host bridge; URL-mode = known gap. |
| Progress | ❌ | Not emitted, not surfaced — no live-progress consumer. |
| Tasks (`taskSupport`) | ⚠️ | `tasks` capability not implemented; `taskSupport=required` tools refused gracefully. |
| Per-request timeout + auto-cancel | ✅ | Bounded `tools/call` + `tools/list`; auto `notifications/cancelled` on timeout. |
| Per-server ping health-check | ✅ opt-in | Periodic `ping`, feeds the circuit breaker. |
| Tool-list-changed cache invalidation | ✅ | Per-server cache evicted on `notifications/tools/list_changed`. |
| Audit logging | ✅ | `mcp.audit` events; arguments SHA-256-hashed, never logged plaintext. |
| Circuit breaker | ✅ | Trips on transport faults; auth challenges deliberately exempt. |
| Secret redaction | ✅ | Outbound errors/logs scrubbed; default-on. |

## Tools and schemas

### JSON Schema dialect

openab-agent supports tool `inputSchema` declared in **JSON Schema draft 2020-12**.

- An **absent `$schema`** is treated as the implied 2020-12 default and passed
  through unflagged.
- A **declared non-2020-12 dialect** is surfaced (not silently relayed): the tool
  is marked `available: false` with an `unavailable_reason` explaining the
  dialect mismatch, visible through both `list_tools` and `describe_tool`. This is
  **advisory only** — it does not hard-refuse a `call`, because arguments are
  never validated locally against the schema, so a tool whose author pinned an
  older dialect remains callable.
- There is **no schema validator**, deliberately. The `inputSchema` is relayed
  straight to the LLM (which tolerates any dialect); a validator for schemas we
  merely forward would be compliance theatre.

### `_meta` keys

openab-agent treats reserved `_meta` keys as **opaque**. It never reads, rewrites,
or makes assumptions about their values — they pass through untouched. It also
does not author `_meta` of its own.

### Icons

Tool/server `icons` are surfaced as raw JSON in `describe_tool` when a server
provides them, but openab-agent **never fetches or renders** them — it is a CLI
gateway with no rendering surface. All icon-consumer obligations (MIME support,
unsafe-scheme rejection, magic-byte validation, same-origin checks) are therefore
**N/A**.

## Authorization

MCP-server OAuth is handled by adopting rmcp's `AuthorizationManager` wholesale.
openab-agent's *own* LLM-provider / legacy-Codex login is a separate subsystem and
is **not** affected by anything in this section.

### What works without bespoke spec code (via rmcp)

| Area | Status | Notes |
|---|---|---|
| PRM / AS-metadata discovery | ✅ | `discover_metadata()` does PRM-first (SEP-985), then RFC 9728 / RFC 8414 / OIDC discovery, with the spec's path-priority order. |
| PKCE (S256) | ✅ | S256 generated unconditionally; discovery enforces `code_challenge_methods_supported ⊇ S256` (rejects an AS without it). |
| RFC 8707 `resource` parameter | ⚠️ | Sent on authorize + token requests — but see the hardcode caveat below. |
| `WWW-Authenticate` step-up | ⚠️ detect-only | Challenge is detected, classified, and surfaced; no automatic reauth-and-retry (see below). |
| HTTPS / loopback enforcement | ✅ | Custom providers must use `https://` endpoints and a loopback-or-`https` redirect. |

**`resource` hardcode caveat.** rmcp hardcodes the RFC 8707 `resource` parameter
to the MCP server's base URL on both authorize and token requests. This means the
parameter is always sent and always equals the server URL — openab-agent can no
longer suppress it per-provider. The earlier behaviour, where the built-in
Anthropic provider deliberately omitted `resource` (its authorization server is
not the MCP server URL), is no longer expressible. This is accepted because the
built-in client ID is environment-gated (a theoretical path in practice) and
flagged for the OAuth-revamp follow-up.

**Step-up is detect-only by design.** When a server answers a tool call with a
401/403 carrying an auth challenge, openab-agent (a) skips the circuit breaker (an
auth challenge is not a transport fault), (b) flags the server as needing auth,
and (c) returns an actionable error telling the operator to run
`mcp login <server>`, including the required scope when the challenge supplied
one. It does **not** silently reauthenticate and retry, because the login flow is
interactive (single-process stdin paste-back) and a background retry cannot mint a
new or upgraded token without a human browser round-trip. This is the realistic
ceiling for an interactive-login client.

### Client registration mechanisms

| Mechanism | Supported | Notes |
|---|---|---|
| Pre-registered client IDs | ✅ | The only supported path. Built-ins inject via env var; custom providers carry an explicit `client_id`. |
| Client ID Metadata Document (CIMD) | ❌ | Not implemented. |
| Dynamic Client Registration (RFC 7591, DCR) | ❌ | Not implemented. |

### Built-in providers and their env vars

There is exactly **one** built-in OAuth provider. Additional built-ins are a code
change, not configuration.

| Provider (`provider:` value) | Client-ID env var | Default scopes |
|---|---|---|
| `anthropic-mcp` | `OPENAB_MCP_ANTHROPIC_CLIENT_ID` | `org:create_api_key`, `user:profile`, `user:inference`, `user:sessions:claude_code`, `user:mcp_servers`, `user:file_upload` |

The client ID is **not** pinned in the repository — a missing env var fails fast
with a clear error rather than falling back to a hard-coded default. Custom
providers supply their own `authorize_url` / `token_url` / `client_id` / `scopes`
via an `oauth:` block in the server config.

### Token storage and known gaps

- Tokens are persisted to `auth.json` under the **`mcp:<server>`** namespace,
  stored as native rmcp `StoredCredentials` (lossless — client ID, granted
  scopes, and vendor-extra fields all survive). The provider/Codex tenant in the
  same file is untouched.
- **Refresh-token rotation fallback**: if an authorization server omits the
  `refresh_token` on a refresh response (permitted by OAuth 2.1 §10.4), the prior
  still-valid token is spliced back in rather than dropped.
- **OS keyring — known gap, no target to exist on.** Tokens live on the
  filesystem (permission-restricted, not in an OS keyring). The primary deploy
  target is Kubernetes/containers, which have no keyring daemon; the k8s-native
  answer to secure storage is a restricted-permission Secret mounted as a file —
  exactly the `auth.json` path already used.
- **Single-process interactive login.** Paste-back is single-invocation; the
  cross-process `--paste` resume was removed by design. This is what caps step-up
  at "bounce, don't auto-retry."
- **Device flow** (RFC 8628) is available for custom providers that advertise a
  device authorization endpoint.

## Sampling

openab-agent serves `sampling/createMessage` requests **text-only**, routing them
back to the agent's own (already-authenticated) LLM provider. When a provider is
wired, it advertises the `sampling` capability (without the `tools` sub-capability)
and converts the request, calls the provider, and returns the result tagged
`assistant` / `endTurn`.

- **Approval gate**: `OPENAB_AGENT_SAMPLING_APPROVAL` (`ask` / `allow` / `deny`,
  **fail-closed** default). `ask` and `deny` reject with a user-rejected result —
  there is no interactive consent UI in a headless agent, so the env var is the
  non-interactive stand-in.
- **Not supported**: tool-enabled sampling (`sampling.tools` is never declared;
  tool-bearing requests are rejected), interactive human-in-the-loop
  review/edit, and per-request rate-limit / tool-loop iteration caps (bundled with
  `sampling.tools`). `modelPreferences` / `maxTokens` / `includeContext` are
  ignored (permitted) — the provider bakes in its model and limits.

## Roots

openab-agent advertises the `roots` capability and answers `list_roots` with a
**static set** computed once at startup: the agent working directory plus a
configured `roots` allow-list. Each candidate is canonicalized (neutralizing
`..` and symlink traversal), kept only if it is a directory, deduplicated, and
emitted as a named `file://` root. There is **no `listChanged`** — the set is
static for the session, so no change notification is ever sent. Consent is
implicit-by-configuration (no interactive prompt).

## Elicitation

Server-initiated **form-mode** elicitation is supported when an ACP host bridge is
wired: openab-agent advertises `elicitation` (form, `schema_validation: false`),
forwards the form to the host as a `session/request_input` request, and maps the
host's structured reply to accept / decline / cancel. If the host channel is
unreachable, the request degrades to *decline* so the server's operation still
completes. `schema_validation: false` is honest non-validation — the schema is
relayed to the host UI, which owns rendering and validation; the reply is not
re-validated locally.

**URL-mode elicitation is a known gap, by design.** A headless agent has no
consent UI, and URL mode's normative obligations *are* that UI (display the full
URL, highlight the domain, warn on Punycode, obtain pre-navigation consent).
Declaring URL support without that surface would both claim non-compliance and add
a phishing vector, so a URL-mode request is rejected. When no host bridge is wired
at all, the `elicitation` capability is not advertised and any elicitation request
is rejected.

## Progress

openab-agent does **not** emit or surface progress. It never populates
`_meta.progressToken` on outbound tool calls, so servers cannot stream progress
back, and any incoming progress notifications are discarded.

This is a structural consequence of LLM-driven dispatch: the LLM blocks on the
**final** tool result, and there is no live-progress consumer surface to render
intermediate updates into. The SDK has all the plumbing; there is simply no
caller, and wiring it up only pays off with a long-running tool *and* a human
watching live.

**What to expect for long-running tools.** A long-running call blocks until the
tool returns — there is no incremental progress display. If it exceeds the
per-server request timeout it is auto-cancelled (a `notifications/cancelled` is
emitted) and surfaces as a timeout error.

## Tasks

The experimental `tasks` capability is **not implemented** — openab-agent declares
no `tasks` capability and issues no task-augmented requests. Tools that declare
`taskSupport: "required"` are handled gracefully rather than failing on the wire:
they are marked `available: false` with a reason, and a `call` against one is
hard-refused locally (audited as `refused`) instead of issuing a request the
server would reject. Tools with `forbidden` or `optional` task support invoke
normally.

## Other reliability mechanisms

- **Per-request timeout + auto-cancel**: both `tools/call` and `tools/list` run
  under a per-server request timeout; on expiry rmcp auto-emits a cancellation
  and the error feeds the circuit breaker. `tools/list` paginates by cursor, each
  page bounded by the same timeout.
- **Per-server ping health-check** (opt-in): a periodic `ping` per connected
  server; failures feed the circuit breaker (catching half-open HTTP
  connections).
- **Tool-list-changed cache invalidation**: a per-server tools cache is evicted
  on that server's `notifications/tools/list_changed`.
- **Capability gating**: tool fetch/call is guarded by the server's advertised
  `tools` capability with a clear error if absent.
- **Audit logging**: `mcp.audit` events at call entry and every exit, with
  arguments **SHA-256-hashed** — never logged in plaintext.
- **Circuit breaker**: consecutive transport faults trip the circuit (cooldown +
  half-open probe); auth challenges are deliberately exempt (they are not
  transport faults). It is failure-protection, not a rate-limiter.
- **Secret redaction**: outbound error and log strings are scrubbed for
  secret-like values by default before they reach the LLM or the operator log.

## How openab-agent compares to other MCP clients

The matrix below positions openab-agent against five other MCP client
implementations. **Honesty note:** only Gemini CLI and OpenAI Codex could be
verified against first-party documentation, and even then only transports and the
existence of OAuth are firmly confirmed; their sampling / roots / progress support
is undocumented. **Hermes, Pi-agent, and OpenClaw could not be grounded against
any authoritative first-party source** (Hermes is an inference/OAuth gateway not
documented as an MCP client; Pi-agent's MCP support is third-party-extension only;
OpenClaw's public claims are self-contradictory and unverifiable). Their cells are
left `?` rather than guessed.

Legend: ✅ verified yes · ⚠️ partial / verified-exists-but-limited · ❌ verified no
· `?` unverifiable.

| Capability | openab-agent | Gemini CLI | OpenAI Codex | Hermes | Pi-agent | OpenClaw |
|---|---|---|---|---|---|---|
| Tools (basic calling) | ✅ | ✅ | ✅ | ? | ? | ? |
| Connection: stdio + streamable HTTP | ✅ | ✅ | ✅ | ? | ? | ? |
| OAuth / authorization | ⚠️ | ⚠️ | ⚠️ | ? | ? | ? |
| RFC 8707 `resource` + PRM discovery | ⚠️ | ? | ? | ? | ? | ? |
| Sampling (`createMessage`) | ✅ text-only | ? | ? | ? | ? | ? |
| Roots | ✅ | ? | ? | ? | ? | ? |
| Elicitation | ⚠️ form-only | ? | ⚠️ | ? | ? | ? |
| Progress notifications | ❌ | ? | ? | ? | ? | ? |

Sources for the verified cells: [Gemini CLI MCP
docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md),
[OpenAI Codex MCP docs](https://developers.openai.com/codex/mcp).

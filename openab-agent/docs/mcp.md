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
| JSON Schema dialect | ✅ validated | `call` args validated against `inputSchema` (`jsonschema`); draft auto-detected (draft 4/6/7/2019-09/2020-12, absent ⇒ 2020-12); uncompilable dialect refused. |
| `_meta` keys | ✅ opaque | Never read or rewritten; passed through untouched. |
| Icon rendering | N/A | No UI surface. |
| Authorization (OAuth) | ⚠️ | rmcp `AuthorizationManager`: PRM/discovery ✅; PKCE S256 generated + **hard-rejected when AS advertises non-S256** ✅; client registration — pre-registered / **DCR public** / **confidential** all supported ✅; step-up **detect + scoped re-login** (`--scope`) ✅; `resource` hardcode caveat ⚠️. |
| Sampling (`createMessage`) | ✅ text-only (provider-conditional) | Advertised **only when an LLM provider is configured**; routed to that provider; env-var approval gate; no `sampling.tools`. With no provider the capability is not advertised and an inbound request is rejected `-32602`. |
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

openab-agent validates `call` arguments against each tool's declared
`inputSchema` (via the `jsonschema` crate) before dispatching `tools/call`.

- The JSON Schema **draft is auto-detected** from the schema's `$schema`
  keyword, so draft 4 / 6 / 7 / 2019-09 / 2020-12 are all honoured. An
  **absent `$schema`** validates under the implied **2020-12** default.
- **Arguments that violate the schema** are refused before any wire traffic; the
  error names every failing instance path so the model can self-correct.
- A **dialect the validator cannot compile** is refused as an unsupported
  dialect (the MCP "handle unsupported dialects gracefully" requirement).
- The **LLM-facing summary is dialect-agnostic**: `list_tools` / `describe_tool`
  do not flag the dialect. (An earlier `available: false` advisory for
  foreign dialects made models decline perfectly callable draft-07 tools, so
  dialect handling now lives only at the `call` path.)

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
| PKCE (S256) | ✅ generate + check | S256 challenge generated unconditionally on every authorize request. rmcp's `validate_server_metadata` only *warns* on a missing/non-S256 advertisement; on top of that, `start_paste_login` **rejects** the login when the AS advertises `code_challenge_methods_supported` without `S256` (refuses to downgrade to `plain`). A server that omits the field is left to the "send PKCE, trust the AS" path. |
| RFC 8707 `resource` parameter | ⚠️ | Sent on authorize + token requests — but see the hardcode caveat below. |
| `WWW-Authenticate` step-up | ⚠️ detect + scoped re-login | Challenge is detected, classified, and surfaced with the required scope; re-login carries it via `mcp login <server> --scope <s>`. No *silent* reauth-and-retry (see below). |
| HTTPS / loopback enforcement | ✅ | Custom providers must use `https://` endpoints and a loopback-or-`https` redirect. |

**`resource` hardcode caveat.** rmcp hardcodes the RFC 8707 `resource` parameter
to the MCP server's base URL on both authorize and token requests. This means the
parameter is always sent and always equals the server URL — openab-agent can no
longer suppress it per-provider. The earlier behaviour, where the built-in
Anthropic provider deliberately omitted `resource` (its authorization server is
not the MCP server URL), is no longer expressible. This is accepted because the
built-in client ID is environment-gated (a theoretical path in practice) and
flagged for the OAuth-revamp follow-up. One further nuance: rmcp emits the raw
`base_url` string (`self.base_url.to_string()`) on the paste-back path, which is
**not** trailing-slash-normalized, so the value is not strictly RFC 8707 §2
canonical there; the hand-rolled device path still canonicalizes the resource
URI.

**Step-up: detect + scoped re-login, no silent retry.** When a server answers a
tool call with a 401/403 carrying an auth challenge, openab-agent (a) skips the
circuit breaker (an auth challenge is not a transport fault), (b) flags the server
as needing auth, and (c) returns an actionable error telling the operator to run
`mcp login <server> --scope <required>`, naming the scope the challenge supplied.
The `--scope` values are merged into the configured set so the new authorize URL
requests the upgraded grant. It does **not** silently reauthenticate and retry,
because the login flow is interactive (single-process stdin paste-back) and a
background retry cannot mint a new or upgraded token without a human browser
round-trip. This is the realistic ceiling for an interactive-login client.

### Client registration

Three modes:

- **Pre-registered client ID** — built-ins inject via env var; custom providers
  carry an explicit `oauth.client_id`.
- **Dynamic Client Registration (RFC 7591, DCR)** — when a custom provider omits
  `oauth.client_id`, `start_paste_login` calls rmcp's `register_client` against the
  discovered `registration_endpoint` and registers a **public** client
  (`token_endpoint_auth_method: none`). The minted `client_id` is persisted inside
  the `StoredCredentials` written at token exchange, so reconnect/refresh reuse it
  with no write-back to `mcp.json`. This only works against servers that advertise
  an **open** registration endpoint (e.g. Notion); a `redirect_uri` is still
  required (DCR registers one). DCR cannot mint a confidential client.
- **Confidential client** — a custom provider may set `oauth.client_secret`
  (`client_secret_basic`/`client_secret_post`). Obtain it by manual
  pre-registration; DCR only produces public clients.

Client ID Metadata Documents (CIMD, SEP-991) remain unimplemented. Per-row
compliance detail is in
[`mcp-spec-alignment.md`](./mcp-spec-alignment.md) (rows 151 / 152 / 169-180).

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

### Token storage

- Tokens are persisted to `auth.json` under the **`mcp:<server>`** namespace,
  stored as native rmcp `StoredCredentials` (lossless — client ID, granted
  scopes, and vendor-extra fields all survive). The provider/Codex tenant in the
  same file is untouched. If a refresh response omits a new `refresh_token`
  (permitted by OAuth 2.1 §10.4), the prior still-valid token is spliced back in
  rather than dropped.
- **Device flow** (RFC 8628) is available for custom providers that advertise a
  device authorization endpoint.

Two known gaps and their rationale are enumerated in
[`mcp-spec-alignment.md`](./mcp-spec-alignment.md): no OS keyring (tokens live on
the permission-restricted filesystem — the k8s deploy target has no keyring daemon,
and a restricted-permission Secret mounted as a file is the canonical equivalent;
row 213 / 437), and single-process interactive login (paste-back is
single-invocation; the cross-process `--paste` resume was removed by design — this
is what caps step-up at "bounce, don't auto-retry").

### Operator commands

The `mcp` subcommand group inspects and authorizes configured servers:

| Command | What it does |
|---|---|
| `mcp list` | Lists configured servers from `mcp.json`. |
| `mcp status` | Per-server one-line state. For non-OAuth servers it prints the in-memory connection icon (○ disconnected / ◐ connecting / ● connected / ◌ needs-auth). For OAuth servers it instead **peeks the credential store** and reports `authed, idle`, `authed, near expiry`, or `◌ … (run mcp login <server>)`. |
| `mcp login <server>` | Runs the interactive paste-back OAuth flow (prints the authorize URL, reads the pasted redirect on stdin, exchanges in the same process). Pass `--scope <scope>` (repeatable) to merge extra scopes into the request for a step-up re-authorization. |
| `mcp login --device <server>` | Device flow (RFC 8628) for servers advertising a device authorization endpoint. |
| `mcp doctor` | End-to-end health check across **all** servers (takes no server argument): live connect attempt plus, for OAuth servers, a cached-token check read from the rmcp `McpCredentialStore`. |

**Why `mcp status` peeks the store.** Each CLI invocation is a fresh process, so
the in-memory connection status of an OAuth server is always `Disconnected` until
something connects in-process — which a bare `status` call doesn't do. Reading the
credential store directly lets `status` report whether a usable token exists
without dialing the server, so an already-authorized server no longer looks dead.
`doctor` is the heavier check that actually connects.

## Verified servers

A non-exhaustive list of MCP servers brought up against openab-agent and what
each exercised. stdio servers spawn via `npx`; HTTP servers are reached directly.
The "Auth" column names the flow actually used.

| Server | Transport | Auth | Status | Notes |
|---|---|---|---|---|
| filesystem (`@modelcontextprotocol/server-filesystem`) | stdio | none | ✅ | |
| sequential-thinking (`@modelcontextprotocol/server-sequential-thinking`) | stdio | none | ✅ | |
| Playwright (`@playwright/mcp`) | stdio | none | ✅ | Headless Chromium; `--isolated` for concurrent sessions and `--image-responses omit` so non-multimodal models aren't fed inline screenshots. |
| Exa (`mcp.exa.ai`) | HTTP | none | ✅ | |
| GitHub Copilot (`api.githubcopilot.com/mcp/`) | HTTP | OAuth — device flow | ✅ | Needs the non-compliant-token-endpoint shim below. |
| Notion (`mcp.notion.com`) | HTTP | OAuth — paste-back | ✅ | AS offers open DCR + public clients + S256; see below. |
| Figma (`mcp.figma.com`) | HTTP | OAuth | ❌ unsupportable | Catalog DCR returns 403 (confidential-client blocker now resolved by A2); see below. |

**GitHub — non-compliant device token endpoint.** GitHub's device token endpoint
returns **HTTP 200 with a JSON error body** (`authorization_pending` / `slow_down`)
instead of the RFC 8628 §3.5-mandated 4xx. The `oauth2` crate treats any 2xx as a
success token and aborts polling on the parse failure, so device login died on the
first poll. openab-agent remaps any success response carrying a top-level `"error"`
field to HTTP 400 so polling continues and terminates correctly on `access_denied`
/ `expired_token`. The `client_id` comes from a self-registered OAuth app — GitHub
exposes no DCR.

**Notion — open registration, public client (DCR verified).** `mcp.notion.com` is
its own authorization server and the smoothest custom OAuth case to date: its
`registration_endpoint` accepts **unauthenticated** RFC 7591 registration (returns
a public `client_id` with `token_endpoint_auth_method: none`), it advertises S256,
and it has no device endpoint, so paste-back is used. This server is what
**Dynamic Client Registration (A1)** was verified against: with **no `client_id`
in the `oauth:` block**, `mcp login notion` discovered the
`registration_endpoint`, called rmcp `register_client`, and minted a fresh public
client ID on the fly — which was then persisted into the rmcp `StoredCredentials`
and reused on reconnect (no re-registration). Pinning a `client_id` by hand still
works and skips registration; DCR is the fallback when the field is absent. rmcp's
hardcoded `resource` parameter happens to equal Notion's expected resource
indicator here, so the caveat in §Authorization is benign for this server.

**Figma — still unsupportable, one blocker left.** `mcp.figma.com` cannot
currently be connected: (1) its DCR endpoint returns **403** — registration is
gated to an approved "MCP Catalog" client allowlist that openab-agent isn't on; and
(2) its token endpoint advertises only `client_secret_basic` / `client_secret_post`
(**no `none`**), i.e. a confidential client with a secret. Blocker (2) is now
addressed by the `oauth.client_secret` field (A2) — a pre-registered confidential
client can supply its secret. Blocker (1) remains: without catalog admission there
is no way to obtain a Figma client at all, so the server stays unsupportable until
Figma allowlists openab-agent. It also has no device endpoint.

## Sampling

openab-agent serves `sampling/createMessage` requests **text-only**, routing them
back to the agent's own (already-authenticated) LLM provider. When a provider is
wired, it advertises the `sampling` capability (without the `tools` sub-capability)
and converts the request, calls the provider, and returns the result tagged
`assistant` / `endTurn`. With no provider the capability is not advertised and an
inbound `createMessage` is rejected with `-32602`.

- **Text-only message conversion**: each inbound message is reduced to plain text.
  A message carrying multiple text content blocks has them **joined with `\n`** so
  block boundaries survive (bare concatenation would fuse the last word of one
  block to the first of the next). Any **non-text** content block (image / audio /
  tool-use / tool-result) is rejected with `-32602` — those ship with the
  `sampling.tools` extension, which is a known gap.
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

## Reliability & operational mechanisms

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
- **Idle eviction** (`idle_ttl_secs`, default 600 = 10 min): a background loop
  disconnects any `Connected` server that has sat idle (no in-flight calls) for
  longer than the TTL; a busy server (calls in flight) is spared. Tools cache is
  retained for fast re-connect. Optional, layered/merged like the rest of the config.
- **Concurrency cap** (`max_concurrent_servers`, default 10): bounds
  simultaneously-connected servers. When a fresh connect would exceed the cap, the
  least-recently-used *idle* server is evicted first; servers with calls in flight
  are spared. Memory-constrained deploys may lower this (e.g. to 3). Optional,
  layered/merged like the rest of the config.
- **Audit logging**: `mcp.audit` events at call entry and every exit, with
  arguments **SHA-256-hashed** — never logged in plaintext.
- **Circuit breaker**: consecutive transport faults trip the circuit (cooldown +
  half-open probe); auth challenges are deliberately exempt (they are not
  transport faults). It is failure-protection, not a rate-limiter.
- **Secret redaction**: outbound error and log strings are scrubbed for
  secret-like values by default before they reach the LLM or the operator log.

## How openab-agent compares to other MCP clients

The matrix below positions openab-agent against the two other MCP clients that
could be **verified against first-party documentation** — Gemini CLI and OpenAI
Codex. Even for those, only transports and the existence of OAuth are firmly
confirmed; their sampling / roots / progress support is undocumented and left `?`
rather than guessed.

Three further clients (Hermes, Pi-agent, OpenClaw) were considered but are **not
in the matrix**: none could be grounded against an authoritative first-party
source (Hermes is an inference/OAuth gateway not documented as an MCP client;
Pi-agent's MCP support is third-party-extension only; OpenClaw's public claims are
self-contradictory). A row of all-`?` columns carries no signal, so they are
omitted rather than listed.

Legend: ✅ verified yes · ⚠️ partial / verified-exists-but-limited · ❌ verified no
· `?` unverifiable.

| Capability | openab-agent | Gemini CLI | OpenAI Codex |
|---|---|---|---|
| Tools (basic calling) | ✅ | ✅ | ✅ |
| Connection: stdio + streamable HTTP | ✅ | ✅ | ✅ |
| OAuth / authorization | ⚠️ | ⚠️ | ⚠️ |
| RFC 8707 `resource` + PRM discovery | ⚠️ | ? | ? |
| Sampling (`createMessage`) | ✅ text-only | ? | ? |
| Roots | ✅ | ? | ? |
| Elicitation | ⚠️ form-only | ? | ⚠️ |
| Progress notifications | ❌ | ? | ? |

Sources for the verified cells: [Gemini CLI MCP
docs](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md),
[OpenAI Codex MCP docs](https://developers.openai.com/codex/mcp).

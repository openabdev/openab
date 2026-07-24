# ADR: OAB MCP Adapter MVP — Gmail and Notion

- **Status:** Proposed
- **Date:** 2026-07-24
- **Author:** chaodu-agent

## 1. Context and Problem Statement

OpenAB already has a native MCP client in `openab-agent`. It supports local stdio
servers and remote Streamable HTTP servers, OAuth, lazy connection, a single
LLM-facing `mcp` meta-tool, and configuration-driven activation. Those decisions
are recorded in [`openab-agent-mcp.md`](./openab-agent-mcp.md).

What is missing is a small, explicit product and integration boundary for
first-party supported external services. Without that boundary, every service
becomes an ad-hoc `mcp.json` entry, authentication and tool-safety expectations
are unclear, and reviewers cannot distinguish an OAB adapter from a generic
third-party MCP server.

This ADR defines **OAB MCP Adapter** as an outbound agent adapter. It connects an
OpenAB agent to configured external MCP servers and exposes their capabilities
through the existing native MCP client. The MVP supports:

- **Notion** via Notion's hosted MCP server.
- **Gmail** via Google's hosted Gmail MCP server, currently a Developer Preview
  and therefore explicitly opt-in and preview-labelled.

This is not a proposal to add a second MCP client, an inbound OAB MCP server, or
native Gmail/Notion REST adapters.

## 2. Goals and Non-Goals

### Goals

- Give OAB a named, documented **MCP adapter** comparable to other OAB
  integrations, while keeping the transport implementation in `openab-agent`.
- Provide tested configuration profiles for Notion and Gmail over Streamable
  HTTP.
- Preserve progressive disclosure: the LLM sees one `mcp` meta-tool and
  discovers provider tools only when needed.
- Reuse the existing MCP OAuth, PKCE, credential storage, timeout, redaction,
  circuit-breaker, and tool-filter mechanisms.
- Make least-privilege and write safety explicit:
  - Notion read tools are the default MVP surface; page/database mutations are
    explicit opt-in tools.
  - Gmail search/read and draft creation are supported; direct send and delete
    are not part of the MVP surface.
- Keep existing deployments unchanged when no MCP server is configured.

### Non-goals

- Implementing an OAB-hosted MCP server for external clients such as Codex or
  Claude Code.
- Replacing OBK for GitHub, AWS, Discord, or other integrations already owned by
  OBK.
- Implementing a native Gmail or Notion REST API adapter in OAB.
- Supporting legacy HTTP+SSE when the provider offers Streamable HTTP.
- Providing organization-wide credential management or a control plane. OAB's
  configured MCP credentials remain agent-local and follow the existing
  `openab-agent` persistence model.
- Automatically enabling Gmail's Developer Preview endpoint in existing or new
  deployments without explicit configuration.

## 3. At a Glance

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ OpenAB                                                                     │
│                                                                          │
│  Channel adapter (Discord / Slack / Gateway)                              │
│        │                                                                    │
│        ▼                                                                    │
│  ACP host ── stdio JSON-RPC ──► openab-agent                              │
│                                  │                                         │
│                                  │ existing `mcp` meta-tool               │
│                                  ▼                                         │
│                         ┌─────────────────────┐                            │
│                         │ OAB MCP Adapter     │                            │
│                         │                    │                            │
│                         │ config + OAuth     │                            │
│                         │ tool discovery     │                            │
│                         │ filtering + audit  │                            │
│                         └─────────┬──────────┘                            │
│                                   │ outbound MCP client                   │
└───────────────────────────────────┼──────────────────────────────────────┘
                                    │ Streamable HTTP + OAuth
             ┌──────────────────────┴────────────────────────┐
             ▼                                               ▼
┌────────────────────────────┐                 ┌────────────────────────────┐
│ Notion hosted MCP           │                 │ Google Gmail hosted MCP     │
│ https://mcp.notion.com/mcp  │                 │ gmailmcp.googleapis.com/... │
│ OAuth; provider permissions │                 │ OAuth; Developer Preview    │
└────────────────────────────┘                 └────────────────────────────┘
```

The direction is intentional: the agent connects **out** to service MCP
servers. OAB does not expose an MCP endpoint in this MVP.

## 4. Terminology and Positioning

- **OAB MCP** is the user-facing feature name.
- **OAB MCP Adapter** is the architecture/component name.
- **MCP server instance** is one configured provider connection, such as
  `notion` or `gmail`.
- **Provider tool** is a tool advertised by that remote MCP server.
- **`mcp` meta-tool** is the existing LLM-facing tool with actions such as
  `list_servers`, `list_tools`, `describe_tool`, and `call`.

MCP is an agent capability adapter, not a channel adapter and not an Ambient
activation mode. Ambient decides when an agent is prompted; MCP decides which
external capabilities the agent can use after it is prompted.

## 5. Prior Art and Industry Research

### 5.1 Existing OpenAB MCP client

[`openab-agent-mcp.md`](./openab-agent-mcp.md) already establishes the relevant
foundation:

- `openab-agent` is an MCP client, not an MCP server.
- `mcpServers` configuration is loaded from global and project
  `.openab/agent/mcp.json` files.
- A configured server is the activation signal; an empty or missing config keeps
  the MCP surface absent.
- Streamable HTTP and stdio are supported; the LLM receives one progressive
  disclosure meta-tool rather than a flat tool list.
- OAuth uses PKCE and persisted credentials, with headless paste-back/device
  flows where the provider supports them.

This ADR adds service profiles and safety expectations; it does not re-specify
that client runtime.

### 5.2 OpenWork

OpenWork's Den provides useful prior art for the product boundary:

- Its native Gmail capabilities use a Google OAuth account and Gmail REST API.
- Its external MCP path treats Notion and similar services as MCP clients,
  discovers provider tools, and proxies calls with member-scoped credentials.
- Its agent-facing endpoint uses progressive discovery instead of exposing every
  provider tool directly.

OAB adopts the external MCP path for Notion. It does not copy OpenWork's Den
control plane or its native Gmail REST adapter; OAB remains a local/agent-side
adapter in this MVP.

References: [OpenWork agent MCP](https://github.com/different-ai/openwork/blob/dev/ee/apps/den-api/src/mcp/agent.ts),
[OpenWork external capabilities](https://github.com/different-ai/openwork/blob/dev/ee/apps/den-api/src/mcp/external-capabilities.ts),
[OpenWork Google Workspace routes](https://github.com/different-ai/openwork/blob/dev/ee/apps/den-api/src/routes/org/google-workspace.ts).

### 5.3 Notion hosted MCP

Notion provides a first-party hosted MCP server at
`https://mcp.notion.com/mcp`, using Streamable HTTP and user OAuth. Its tools
are intentionally agent-oriented rather than a one-to-one dump of REST
endpoints. The documented surface includes search, fetch, page create/update,
comments, database queries, and async task status.

Notion's official guidance also states that hosted MCP requires user-based OAuth
and does not support bearer-token authentication for headless service use. This
makes it a good MVP remote connector, but an operator must complete an
interactive login for each configured account.

References: [Notion connection guide](https://developers.notion.com/guides/mcp/get-started-with-mcp),
[Notion supported tools](https://developers.notion.com/guides/mcp/mcp-supported-tools),
[Notion hosted MCP design](https://www.notion.com/blog/notions-hosted-mcp-server-an-inside-look).

### 5.4 Gmail hosted MCP

Google provides `https://gmailmcp.googleapis.com/mcp/v1` as a Gmail remote MCP
server. The official documentation currently labels it **Developer Preview**.
The documented setup enables Gmail API and Gmail MCP API, configures OAuth, and
requests `gmail.readonly` and `gmail.compose`. The documented tools include
thread/message search and retrieval, labels, and draft creation.

Because the service is preview-only, Gmail support is an explicit opt-in profile
in this MVP. Direct sending and deletion are not exposed by the OAB profile.
A future native Gmail adapter remains possible if the hosted MCP contract is
not stable enough for production.

Reference: [Google Gmail MCP setup](https://developers.google.com/workspace/gmail/api/guides/configure-mcp-server).

### 5.5 OpenClaw and Hermes Agent

The repository contribution guidelines require OpenClaw and Hermes Agent as
prior art for runtime integrations.

- **OpenClaw** is primarily a multi-channel gateway. Its MCP work is useful for
  configuration and channel-to-tool bridging, but it is not a direct model for
  the native `openab-agent` client. OAB therefore keeps channel adapters and
  the outbound MCP adapter as separate layers.
- **Hermes Agent** provides relevant MCP lifecycle patterns: per-server state,
  OAuth handling, failure isolation, and circuit breaking. OAB already has
  equivalent foundations in `openab-agent`; this MVP adds no second lifecycle
  manager.

References: [OpenClaw](https://github.com/openclaw/openclaw),
[Hermes Agent MCP implementation](https://github.com/NousResearch/hermes-agent).

## 6. Proposed Solution

### 6.1 Adapter boundary

Add OAB MCP as a documented adapter profile on top of the existing
`McpRuntimeManager`:

```text
McpRuntimeManager
  └── McpAdapter instance per configured server
        ├── notion → hosted MCP + OAuth
        └── gmail  → hosted MCP + OAuth (preview)
```

The adapter owns connection concerns only:

- endpoint and transport selection;
- OAuth discovery, PKCE, login, refresh, and credential namespace;
- `tools/list` and tool-schema caching;
- configured include/exclude tool filters;
- timeout, cancellation, circuit breaker, and redacted errors;
- dispatch of exact provider tool names and arguments.

Provider business logic remains at the provider MCP server. OAB must not
reimplement Notion or Gmail REST operations as part of this MVP.

### 6.2 Configuration and activation

Keep the existing `openab-agent` configuration contract rather than introducing
a duplicate top-level TOML section:

- global: `~/.openab/agent/mcp.json`;
- project: `.openab/agent/mcp.json`;
- project entries override global entries with the same server name;
- no configured servers means no MCP meta-tool is injected;
- declaring a server is the explicit opt-in activation signal.

Illustrative configuration:

```json
{
  "mcpServers": {
    "notion": {
      "type": "http",
      "url": "https://mcp.notion.com/mcp",
      "oauth": {
        "discovery": true,
        "discovery_allowlist": ["*.notion.com"]
      },
      "tool_filter": {
        "include": [
          "notion-search",
          "notion-fetch",
          "notion-get-async-task"
        ]
      }
    },
    "gmail": {
      "type": "http",
      "url": "https://gmailmcp.googleapis.com/mcp/v1",
      "oauth": {
        "discovery": true,
        "discovery_allowlist": ["*.google.com", "*.googleapis.com"],
        "scopes": [
          "https://www.googleapis.com/auth/gmail.readonly",
          "https://www.googleapis.com/auth/gmail.compose"
        ]
      },
      "tool_filter": {
        "include": [
          "search_threads",
          "get_thread",
          "get_message",
          "create_draft"
        ]
      }
    }
  }
}
```

The exact OAuth client registration fields remain deployment/provider-specific.
They must not be committed with credentials. If a provider requires a
pre-registered client, operators use the existing `oauth.client_id`,
`oauth.client_secret`, and environment interpolation facilities.

The profile examples are deliberately conservative. A deployment may add
provider tools after reviewing their schemas and side effects.

### 6.3 Discovery and execution

The existing meta-tool flow remains authoritative:

```text
mcp(action="list_servers")
  → notion (http, idle), gmail (http, needs login)

mcp(action="list_tools", server="notion")
  → provider tool names and descriptions

mcp(action="describe_tool", server="notion", tool="notion-fetch")
  → exact input schema

mcp(action="call", server="notion", tool="notion-fetch", arguments={...})
  → provider CallToolResult
```

The LLM must use exact names and schemas returned by discovery. It must not
invent provider tools or arguments. An MCP `tools/list_changed` notification
invalidates the cache for that server.

### 6.4 MVP capability profiles

#### Notion

- Stable hosted endpoint: `https://mcp.notion.com/mcp`.
- User OAuth; each configured account is authorized by a human.
- Default profile: search, fetch, and async-status read operations.
- Page/database/comment mutations require an explicit tool-filter change and
  normal confirmation policy.
- File upload is out of scope because Notion's hosted MCP documentation does
  not currently support it.

#### Gmail

- Hosted endpoint: `https://gmailmcp.googleapis.com/mcp/v1`.
- Developer Preview; disabled unless explicitly configured.
- OAuth scopes limited to `gmail.readonly` and `gmail.compose`.
- MVP profile: search/read threads and messages, plus create drafts.
- Direct send, permanent delete, settings changes, and unrestricted mailbox
  mutation are out of scope.
- Draft results must be presented as drafts for user review; the adapter must
  not claim that a message was sent.

### 6.5 Credential and session model

- HTTP MCP servers use the existing `openab-agent` OAuth manager and
  namespaced credential store (`mcp:<server-name>`).
- Refresh tokens remain on the agent's protected persistent filesystem and are
  never sent through chat, inserted into prompts, or written to logs.
- The existing headless login flow is used for remote servers: device flow when
  advertised, otherwise PKCE paste-back flow.
- A server is connected lazily on first discovery or call; one failing server
  must not prevent the agent from starting or using another server.
- Existing per-server timeout, cancellation, idle eviction, and circuit breaker
  behavior applies without an adapter-specific retry loop.

### 6.6 Safety policy

The adapter treats remote content as untrusted data:

- Email bodies, Notion pages, comments, and search results must never be
  interpreted as OAB system instructions.
- Read-only tools are preferred for the default profile.
- Mutation tools require explicit configuration and must preserve the provider's
  own authorization checks.
- Gmail `create_draft` is the only write-like Gmail operation in the MVP; no
  send capability is enabled.
- Tool arguments are validated against provider-declared schemas before call.
- Provider URLs and OAuth discovery are restricted to HTTPS and explicit
  allowlists according to the existing MCP config validation.
- Stdio servers continue to use the existing environment scrubbing; the adapter
  must not pass OAB channel tokens or unrelated secrets to child processes.
- Errors returned to the LLM and audit logs remain redacted.

## 7. Why This Approach

This approach uses the provider's maintained MCP contract while keeping OAB's
existing client/runtime small:

- Notion already provides a production-oriented hosted MCP server and
  agent-friendly tools; duplicating its REST API would create maintenance and
  schema drift.
- Gmail's official server is available for an opt-in preview integration. The
  profile makes its preview status and limited scopes visible instead of hiding
  the risk behind a native adapter.
- Reusing the existing MCP client avoids a second OAuth store, transport stack,
  meta-tool, or server lifecycle implementation.
- Tool filtering and progressive discovery prevent the model context from being
  flooded with provider tools and create an explicit safety boundary.
- Existing deployments are unchanged: no `mcp.json` means no MCP runtime and no
  new network calls.

The trade-off is dependency on provider MCP availability, OAuth behavior, and
remote tool-schema stability. Those risks are acceptable for Notion and for an
explicitly preview-labelled Gmail profile; they are not acceptable grounds for
silently enabling either provider.

## 8. Alternatives Considered

### A. Add native Gmail and Notion REST adapters

Rejected for this MVP. It duplicates provider API clients, OAuth scope mapping,
response normalization, and rate-limit behavior already implemented by the
providers. It remains a possible Gmail follow-up if Google's hosted MCP stays
preview-only or lacks required production controls.

### B. Let each backing coding CLI configure Notion/Gmail directly

Rejected as the OAB default. It produces different auth, tool filtering,
credential persistence, and diagnostics for each CLI. It can remain a manual
escape hatch for operators who intentionally configure a server outside OAB.

### C. Add an inbound OAB MCP server

Rejected as out of scope. This MVP is an outbound agent adapter. An inbound
server would let external coding clients call OAB workflows and requires a
separate authentication, tenancy, and authorization design.

### D. Flatten all provider tools into the agent's top-level tool list

Rejected. The existing `mcp` meta-tool and lazy discovery preserve the agent's
small prompt and isolate provider schema drift. This is already the accepted
OpenAB MCP client architecture.

### E. Add a new top-level `[mcp]` TOML section

Rejected for the MVP because `openab-agent` already owns and documents layered
`.openab/agent/mcp.json` configuration. A future broker-level configuration
facade may point to or generate that file, but it should not create a second
source of truth.

## 9. Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Gmail hosted MCP is Developer Preview | Contract or availability changes | Explicit opt-in; preview label; limited tools/scopes; native adapter remains a follow-up |
| OAuth requires a human browser flow | Headless deployment cannot silently connect | Existing paste-back/device flows; actionable `mcp login` instructions; no false ready state |
| Provider tool/schema drift | Calls fail or model guesses stale arguments | Cache invalidation on `tools/list_changed`; exact-name/schema discovery; `mcp doctor` |
| Prompt injection in email/page content | Agent may perform unintended actions | Treat provider data as untrusted; least-privilege filters; no Gmail send/delete |
| Token leakage | Account compromise | PKCE, protected token store, env scrubbing, redacted logs, no credentials in prompts |
| Remote MCP outage/rate limit | Agent task failure or latency | Per-server timeout, circuit breaker, bounded retries, provider error surfaced accurately |
| Excessive provider tool context | Poor tool selection and token cost | Single meta-tool, lazy `list_tools`/`describe_tool`, include filters |
| Notion/Gmail account mismatch | Wrong mailbox/workspace action | Show server name and auth state; require explicit login per configured server; never infer identity from content |

## 10. Rollout Plan

1. **Documentation/profile slice:** land this ADR and checked-in configuration
   examples; no behavior change for existing deployments.
2. **Notion MVP:** validate OAuth login, discovery, read tools, and one explicit
   page mutation in a controlled test workspace.
3. **Gmail preview MVP:** validate Google OAuth, search/read, and draft creation
   with a test mailbox; keep the profile opt-in and clearly preview-labelled.
4. **Operational hardening:** add provider-specific smoke checks and document
   reconnect, rate-limit, and schema-drift remediation.
5. **Follow-up decision:** decide whether Gmail should remain hosted-MCP based on
   preview stability or move to a native Gmail adapter.

## 11. Validation

### Documentation and configuration

- Verify all relative ADR links resolve from `docs/adr/`.
- Parse the JSON examples and validate that profile fields match the existing
  `openab-agent` MCP config schema.
- Confirm existing configuration with no `mcp.json` remains unchanged.

### Automated checks

- Unit-test config layering and server activation with zero, one, and two
  servers.
- Unit-test include/exclude filters so Gmail send/delete-like tools cannot enter
  the default profile.
- Unit-test OAuth URL/discovery allowlist validation and secret redaction.
- Mock Streamable HTTP `initialize`, `tools/list`, `tools/list_changed`, and
  `tools/call` for both profiles.
- Test one provider failure does not prevent the other provider from connecting.

### Manual integration checks

- Notion: `mcp login notion`, list tools, search/fetch a test page, and verify a
  mutation is blocked until explicitly enabled.
- Gmail: `mcp login gmail`, search a test mailbox, fetch a thread, create a draft,
  and verify no send operation is exposed.
- Run `openab-agent mcp doctor` for each configured server.

The implementation PR must run the repository validation commands required by
`AGENTS.md`:

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

For this docs-only ADR proposal, no code or Helm behavior changes are expected;
provider smoke tests are acceptance criteria for the follow-up implementation PR.

## 12. Decision Summary

Adopt **OAB MCP Adapter** as a first-class outbound agent adapter implemented on
top of the existing `openab-agent` MCP client.

The MVP supports:

- Notion hosted MCP with user OAuth and conservative read-first tooling.
- Gmail hosted MCP as an explicit, limited-scope Developer Preview integration
  for search/read and draft creation.

Configuration presence remains the opt-in signal. The MVP does not add an OAB
MCP server, a second MCP runtime, a native Gmail/Notion REST client, or a new
TOML source of truth.

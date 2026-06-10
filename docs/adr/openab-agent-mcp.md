# ADR: openab-agent — MCP Client Support

## 1. Context & Motivation

`openab-agent` is the native Rust coding agent shipped with OpenAB (Cargo workspace member `openab-agent/`, introduced 2026-05-26 via PR #924, targeted at the v0.8.4-beta series). Its `docs/adr/openab-agent.md` charter commits to a small surface: 4 built-in tools (`read`, `write`, `edit`, `bash`), a ~500-token system prompt, no LLM SDK dependency, multi-model via thin HTTP. PR #955 added `Skills` support (`openab-agent/src/skills.rs`, 224 LOC, zero new crate dependencies) as the first extension mechanism — descriptor-only injection plus on-demand load via the existing `read` tool.

The agent currently has **no MCP (Model Context Protocol) client**. This ADR proposes one.

### 1.1 Why MCP for openab-agent

- **Ecosystem leverage.** Every Postgres/GitHub/Figma/Jira/Slack integration users will ask for already exists as an MCP server (mcpbundles.com tracks ~9k tools across ~1.4k providers as of 2026-Q2). Re-implementing each as a Skill or built-in tool is duplicative.
- **Parity with peer agents.** Claude Code, Codex CLI, Cursor, Cline, Goose, opencode, OpenHands, Kiro, Junie, Roo Code all ship MCP clients. Users coming from any of these expect `mcpServers` config to "just work".
- **Skills cannot replace MCP.** Per Anthropic's framing — **Skills = procedural (how to do); MCP = connectivity (where data/tools live)**. Skills wrap CLI tools; MCP handles network, auth, streaming, server-side state.

### 1.2 Why now

Skills landed in PR #955. The repo's design pattern for "first-tier-but-tiny" extension is now established. MCP is the natural next layer.

### 1.3 Prior internal attempts

Four MCP PRs to upstream `openabdev/openab` have closed without merging:

| PR | Title | State | Scope |
|---|---|---|---|
| #329, #330 | `feat(mcp): inject per-user MCP servers from Discord profiles into ACP sessions` | CLOSED | Broker forward |
| #345 | `feat: inject per-user MCP servers into ACP sessions` | CLOSED | Broker forward |
| #903 | `feat(agent): forward configured MCP servers` | CLOSED | Broker forward |

All four targeted the broker layer — pass MCP server config through to the backing CLI (Claude Code / Codex / Cursor) and let that CLI handle MCP. **None addressed native MCP support inside `openab-agent` itself.** This ADR is scoped to the native agent.

Issue #753 remains open and is broker-side (`[agent].inherit_cloud_mcp_servers` opt-out). This ADR does not change broker behavior.

---

## 2. Goals & Non-Goals

### In scope

- MCP **client** support inside `openab-agent`
- Transports: stdio (local servers — Anthropic reference, npm/pypi community) and Streamable HTTP (vendor-hosted SaaS — Atlassian, Figma, Linear, Notion, Sentry, etc.). HTTP+SSE intentionally omitted — superseded by MCP spec 2025-11-25 and actively sunset by vendors (Atlassian deadline 2026-06-30). See §3.8 for landscape.
- OAuth login flow for MCP servers requiring it
- Per-session lifecycle with idle eviction
- Per-session config refresh — new ACP session re-reads `mcpServers` from disk (no file watcher, no mid-session reload; openab spawns short-lived sessions per thread so process restart is rarely needed)
- Progressive-disclosure tool surface (single meta-tool, not flat fan-out)
- Reuse of existing `src/auth.rs` PKCE / TokenStore where possible

### Out of scope

- MCP **server** functionality (host only)
- WASM / cdylib plugin runtime
- Sidecar / out-of-process MCP bridge
- Per-thread MCP isolation (broker concern, not agent)
- Replacing Skills (Skills and MCP coexist)

---

## 3. Prior Art Survey

Per `docs/adr/pr-contribution-guidelines.md`, OpenClaw and Hermes Agent are the mandatory references for architectural PRs. OpenClaw was evaluated and found **not applicable to this ADR**: it is a multi-channel messaging gateway (chat platforms ↔ MCP), not a coding agent. Its substantial MCP code (~2,900 LOC across `src/agents/mcp-*`, `src/config/mcp-*`, `src/mcp/`) addresses channel bridging rather than agent-side tool calling. The closer comparison for a coding-agent MCP client is **opencode (§3.2)**, included in addition to Hermes Agent.

Five projects are surveyed below. Each contributes a design pattern the chosen architecture borrows from:

| § | Project | Borrowed pattern |
|---|---|---|
| 3.1 | Hermes Agent | Circuit breaker (per-server fail threshold + cooldown) |
| 3.2 | opencode | Per-server status enum + RFC 7591 dynamic OAuth |
| 3.3 | pi-mcp-adapter | Single `mcp` meta-tool with action dispatch (progressive disclosure) |
| 3.4 | Goose | MCP-as-primary-extension validation in a Rust codebase |
| 3.5 | OpenHands | `filter_tools_regex` per-server tool scoping |

### 3.1 Hermes Agent (mandatory reference)

- Repo: https://github.com/NousResearch/hermes-agent (Python, Apache 2.0)
- MCP module: ~5,175 LOC across 3 files (`mcp_tool.py` + 2 OAuth modules)
- SDK: official `mcp==1.26.0`
- Transports: stdio + Streamable HTTP + SSE
- Tool naming: `mcp_{server}_{tool}` (single-underscore separators, no `__` boundary marker)
- Lifecycle: per-server long-lived `asyncio.Task` on dedicated background event loop
- Lazy loading: eager connect, but background-thread discovery with 0.75s join — non-blocking
- Hot reload: mtime-poll on `~/.hermes/config.yaml` + `/reload-mcp` slash command
- OAuth: mtime-based disk-watch for cross-process token refresh
- **Notable**: ships a real circuit breaker — threshold 3 failures / 60s cooldown / half-open probe state. The only project surveyed that does so.

### 3.2 opencode (anomalyco/opencode, formerly sst/opencode)

- Repo: https://github.com/anomalyco/opencode (TypeScript, MIT) — `sst/opencode` 301-redirects here after org transfer
- **Closest coding-agent comparison to openab-agent**
- MCP module: ~1,664 LOC across 5 files (`mcp/`, `auth.ts`, OAuth provider/callback, config)
- SDK: `@modelcontextprotocol/sdk@1.27.1`
- Transports: stdio + Streamable HTTP + SSE
- Tool naming: `{sanitized_client}_{sanitized_tool}` (single underscore)
- Lifecycle: shared singleton service via Effect `Layer`; one `Client` per server
- Lazy loading: eager connect with `concurrency: "unbounded"`; per-server status union prevents one bad server from crashing others
- Hot reload: subscribes to MCP spec's `notifications/tools/list_changed`; **no file watcher** for config — config change still requires restart
- OAuth: RFC 7591 dynamic client registration, callback `http://127.0.0.1:19876/mcp/oauth/callback`, EffectFlock cross-process locking on token store
- **Known issues** (cited as architectural cautionary tales): #11868 (113 GB virtual-memory leak, Windows v1.1.21), #7261 (heap not released + MCP orphan processes, v1.1.6), #13041 (per-session MCP+LSP duplication across concurrent sessions) — all rooted in child-process lifecycle, not protocol code

### 3.3 pi-mcp-adapter

- Repo: https://github.com/nicobailon/pi-mcp-adapter (TypeScript, MIT)
- An out-of-tree extension for the Pi coding agent (`pi.extensions`) — Pi itself has **no native MCP**
- MCP module: ~3,661 LOC (server-manager, proxy-modes, direct-tools, OAuth)
- SDK: `@modelcontextprotocol/sdk@^1.25.1` + `@modelcontextprotocol/ext-apps@^1.2.2`
- Transports: stdio + Streamable HTTP + SSE
- **Notable — the reason this is cited**: ships a **single `mcp` meta-tool** with sub-actions (`connect`, `describe`, `search`, `list`, `call`, `status`). All MCP capability is exposed through this one tool. Lazy connect happens inside `lazyConnect()` on first action that needs it. This is the **progressive-disclosure pattern** this ADR adopts.

### 3.4 Goose (block / aaif-goose)

- Repo: https://github.com/block/goose → https://github.com/aaif-goose/goose (Rust, Apache 2.0)
- **Most relevant precedent: a Rust coding agent built around MCP**
- Launched Jan 2025 with MCP as the *only* extension surface (no first-party plugin API to retrofit)
- Hand-rolled `mcp-client` crate (predated official Rust SDK)
- Per-session `Agent` owns an `ExtensionManager` that spawns MCP servers (stdio/SSE) as child processes
- Tools flattened into one namespace; extension name used as prefix to avoid collisions
- Supports `tools/list_changed` for hot reload
- Precedent for a Rust agent shipping MCP as the primary extension surface without WASM / cdylib / sidecar plumbing.

### 3.5 OpenHands (All-Hands-AI)

- Repo: https://github.com/OpenHands/OpenHands (Python, MIT)
- SDK: FastMCP (jlowin/fastmcp), not the reference SDK
- **Notable**: per-agent `filter_tools_regex` config — subset a server's tools without modifying the server. OAuth tokens cached under `~/.fastmcp/oauth-mcp-client-cache/` with auto-refresh; explicit "incompatible with headless" caveat for browser-based auth.
- Cited for OAuth + tool-surface scoping patterns where Hermes/opencode/Pi are weaker.

### 3.6 Comparison matrix

| | Hermes | opencode | pi-mcp-adapter | Goose | OpenHands |
|---|---|---|---|---|---|
| Language | Python | TS | TS | Rust | Python |
| SDK | mcp 1.26 | sdk 1.27 | sdk 1.25 | hand-rolled | FastMCP |
| Transports | stdio+HTTP+SSE | stdio+HTTP+SSE | stdio+HTTP+SSE | stdio+SSE | stdio+HTTP |
| Tool naming | `mcp_s_t` | `s_t` | configurable | ext-prefix | filter |
| Lifecycle | per-srv task | shared singleton | per-ext + idle 10m | per-session ExtensionMgr | per-agent |
| Lazy connect | no | no | ✅ meta | no (eager) | no |
| Hot reload | mtime+cmd | `tools/list_changed` | session boundary | `tools/list_changed` | no |
| OAuth | mtime disk-watch | RFC7591 + Flock | PKCE+auto | ? | FastMCP cache |
| Circuit breaker | ✅ 3/60s | no | partial | no | no |
| LOC | ~5,175 | ~1,664 | ~3,661 | unmeasured | unmeasured |

### 3.7 Skills vs MCP — industry research

Anthropic positions the two as **complementary**, not competing. The 2025-2026 consensus across practitioner blogs (Simon Willison, Anthropic engineering, StackOne) converged on:

```
  Skills                                    MCP
  ──────                                    ────
  Procedural knowledge                      Live connectivity
  Markdown + YAML frontmatter               Protocol spec + SDK
  ~100 tokens/skill in prompt               10K-17K tokens/server in prompt
  Body lazy-loaded via read tool            All tool schemas eagerly loaded
  Local file                                Server (process or HTTP endpoint)
  No auth, no lifecycle                     OAuth, lifecycle, transports
  Open standard (Dec 2025)                  Linux Foundation steward (late 2025)
```

**Adoption**: no major OSS coding agent has rejected MCP in favor of Skills-only (or vice versa). All 11 surveyed agents (Claude Code, Codex CLI, Gemini CLI, Cursor, Cline, Goose, opencode, Junie, Kiro, Roo, GitHub Copilot agent-mode) support both.

**Cost data**: large MCP server collections have been documented consuming substantial context budget — StackOne benchmarks Sonnet 4.6 at 42% tool-selection accuracy on the unmodified MCP surface vs 80% with their Code Mode wrapper, motivating the spec-level fix in MCP SEP-1576 ("Mitigating Token Bloat in MCP") which proposes progressive disclosure (**not yet ratified**).

**Implication for this ADR**: progressive disclosure is not optional for openab-agent. The agent's design principle commits to a ~500-token system prompt; a naïve flat MCP integration would 30× that budget. Skills' descriptor-only injection pattern is the precedent.

### 3.8 Transport landscape & SaaS MCP server adoption

MCP defines three transport profiles. Their 2026-Q2 status:

| Transport | Spec status | Where it lives |
|---|---|---|
| **stdio** | Stable | Local child process — Anthropic reference servers, npm/pypi community packages |
| **Streamable HTTP** | Current (MCP spec 2025-11-25), supersedes HTTP+SSE | Vendor-hosted SaaS endpoints |
| **HTTP+SSE** | Deprecated by spec 2025-11-25; vendor sunsets in progress | Legacy fixtures — Atlassian sunsets 2026-06-30 |

```
   ┌──────────────────────────────── MCP Server Universe ─────────────────────────────────┐
   │                                                                                      │
   │   ┌─────────────────────────────┐         ┌────────────────────────────────────┐     │
   │   │  LOCAL  (majority of registry) │      │  REMOTE (vendor SaaS, growing)     │     │
   │   │                             │         │                                    │     │
   │   │  Transport: stdio           │         │  Transport: Streamable HTTP        │     │
   │   │                             │         │                                    │     │
   │   │  filesystem  sqlite         │         │  Atlassian  Figma   Linear         │     │
   │   │  postgres    puppeteer      │         │  Notion     Sentry  Supabase       │     │
   │   │  github      fetch          │         │  HubSpot    Slack   Stripe         │     │
   │   │  time        gitlab         │         │  Cloudflare Vercel  Neon  ...      │     │
   │   │  ...                        │         │                                    │     │
   │   └─────────────────────────────┘         └────────────────────────────────────┘     │
   │                                                                                      │
   │   ┌────────────────────────────────────────────────────────────────────────────┐     │
   │   │  LEGACY (deprecated, vendor sunsets in progress)                           │     │
   │   │  Transport: HTTP+SSE                                                       │     │
   │   │  e.g. Atlassian https://mcp.atlassian.com/v1/sse (off 2026-06-30)          │     │
   │   └────────────────────────────────────────────────────────────────────────────┘     │
   │                                                                                      │
   └──────────────────────────────────────────────────────────────────────────────────────┘
```

#### Local stdio servers (representative sample)

Anthropic reference + community packages. All ship as `command + args`; no network endpoint.

| Server | Implementation | Distribution |
|---|---|---|
| `mcp-server-filesystem` | Node | `@modelcontextprotocol/server-filesystem` (npm) |
| `mcp-server-sqlite` | Python | `mcp-server-sqlite` (pypi) |
| `mcp-server-postgres` | Python | `mcp-server-postgres` (pypi) — local DB |
| `mcp-server-puppeteer` | Node | `@modelcontextprotocol/server-puppeteer` (npm) |
| `mcp-server-github` | Go / Node | `github-mcp-server` (binary) / `@modelcontextprotocol/server-github` (npm) |
| `mcp-server-fetch` | Python | `mcp-server-fetch` (pypi) |
| `mcp-server-time` | Rust | `mcp-server-time` (cargo) |
| `mcp-server-gitlab` | Node | `@modelcontextprotocol/server-gitlab` (npm) |

**Container-image caveat for headless deployments**: Node/Python stdio servers require the corresponding interpreter (`node`, `python3`, `uvx`, `npx`) in the image. The openab base image ships none. Operators running openab-agent in headless environments (Fargate, Kubernetes pods, CI) must either bake the interpreter into a derived image or limit `mcpServers` to Go/Rust binaries (column above). A misconfigured server fails in isolation per §5.9.

#### Vendor-hosted SaaS servers — all Streamable HTTP

Survey of mainstream public endpoints (2026-Q2). Every active vendor endpoint surveyed is Streamable HTTP. The Atlassian SSE URL is the lone holdout and has a published sunset date.

| Vendor | Endpoint | Transport | Notes |
|---|---|---|---|
| Atlassian (Rovo) | `https://mcp.atlassian.com/v1/mcp` | Streamable HTTP | Legacy SSE at `/v1/sse` sunset **2026-06-30** |
| Figma | `https://mcp.figma.com/mcp` | Streamable HTTP | OAuth via Figma account |
| Linear | `https://mcp.linear.app/mcp` | Streamable HTTP | OAuth |
| Notion | `https://mcp.notion.com/mcp` | Streamable HTTP | OAuth |
| Sentry | `https://mcp.sentry.dev/mcp` | Streamable HTTP | OAuth |
| Supabase | `https://mcp.supabase.com/mcp` | Streamable HTTP | OAuth |
| HubSpot | `https://mcp.hubspot.com/anthropic` | Streamable HTTP | OAuth |
| Slack | (vendor-hosted) | Streamable HTTP | OAuth |
| Stripe | hosted (see Stripe MCP docs for current path) | Streamable HTTP | API key |
| Cloudflare | multiple endpoints under `*.mcp.cloudflare.com` | Streamable HTTP | OAuth (workers/dns/r2/...) |
| Vercel | `https://mcp.vercel.com/` | Streamable HTTP | OAuth |
| Neon | `https://mcp.neon.tech/` | Streamable HTTP | OAuth |

**Cover map**: stdio + Streamable HTTP covers all mainstream public MCP endpoints surveyed as of 2026-Q2. SSE-only deployments are legacy fixtures with vendor sunsets in progress; deferred to a hypothetical v2.

---

## 4. Design Decision

### 4.1 Architectural alternatives compared

**Alternative A — Naïve flat in-core.** Every MCP tool from every connected server becomes a top-level entry in `tool_definitions()`. Surface explodes from 4 → 150+ tools; system prompt grows ~500 → ~17,000 tokens (5 servers × ~20 tools each, ~160 tokens per descriptor). Hermes Agent and opencode both pay this cost; StackOne benchmarks (§3.7) show tool-selection accuracy drops sharply under naïve flat surfaces.

**Alternative B — Sidecar / plugin process.** Spawn a separate `openab-mcp-bridge` binary; agent core has no `rmcp` dependency; communicate via stdio JSON-RPC. RAM saving is 1-2 MB on a 15-40 MB baseline — noise — but the bridge process itself adds ~15 MB and inherits opencode's documented sidecar failure modes (#11868 113 GB leak / #7261 orphan processes / #13041 per-session duplication). Cost ≫ benefit (see §7).

**Alternative C — CHOSEN: in-core `rmcp` + progressive-disclosure meta-tool.** `rmcp` enters `Cargo.toml`. Tool surface grows by exactly **1 tool**: `mcp`. All MCP capability (server enumeration, tool discovery, invocation, status) flows through that single tool's `action` field. System prompt grows ~500 → ~600 tokens (+100 for the meta-tool blurb).

### 4.2 Why C honors openab-agent design principles

| Principle (`docs/adr/openab-agent.md` §2) | A (flat) | B (sidecar) | **C (chosen)** |
|---|:---:|:---:|:---:|
| Minimal tool surface (4 tools) | ⛔ 150+ | ✅ 4 | ✅ 5 |
| Tiny system prompt (~500 tokens) | ⛔ ~17K | ✅ ~500 | ⚠️ ~600 (+100 over budget; accepted as smallest viable surface) |
| No SDK dependency | ⛔ rmcp | ✅ none | ⚠️ rmcp (+1-2 MB binary, see §7) |
| Multi-model | ✅ | ✅ | ✅ |

C concedes the "no SDK dependency" principle for a 1-2 MB binary cost. §7 shows that cost is dominated by child-process RAM (5-80 MB per server, depending on implementation language) regardless of architecture, so the concession is dwarfed by usage cost.

### 4.3 Symmetry with Skills (PR #955)

Skills is openab's existing "first-tier-but-tiny" extension. The mapping is exact:

```
┌────────────────────────────┬─────────────────────────────────────┐
│  Skills (224 LOC, in-core) │  MCP (proposed, in-core)            │
├────────────────────────────┼─────────────────────────────────────┤
│ Inject metadata only       │ Inject 1 meta-tool only             │
│ (name + description)       │ (name + action sketch)              │
├────────────────────────────┼─────────────────────────────────────┤
│ Body load via `read` tool  │ Server connect via `mcp` tool       │
│ on agent's demand          │ on agent's demand (lazy connect)    │
├────────────────────────────┼─────────────────────────────────────┤
│ ~100 tokens / 10 skills    │ ~100 tokens / N servers             │
├────────────────────────────┼─────────────────────────────────────┤
│ No new crate dep           │ Adds rmcp (1-2 MB binary delta)     │
└────────────────────────────┴─────────────────────────────────────┘
```

Skills' authors weighed "simple in-core mechanism vs plugin abstraction" and chose in-core. The same trade-off applies to MCP: plugin abstraction is ~10× the complexity for negligible RAM saving.

---

## 5. Detailed Design

### 5.1 Tool surface (4 + 1)

```
openab-agent/src/tools.rs::tool_definitions() returns 5 entries:

  [ "read"  ] ─── existing, unchanged
  [ "write" ] ─── existing, unchanged
  [ "edit"  ] ─── existing, unchanged
  [ "bash"  ] ─── existing, unchanged
  [ "mcp"   ] ─── NEW
```

### 5.2 The `mcp` meta-tool schema

```jsonc
{
  "name": "mcp",
  "description": "Interact with configured MCP servers. Use action='help' for usage.",
  "input_schema": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["help", "list_servers", "list_tools",
                 "describe_tool", "call", "status",
                 "login", "complete_login"]
      },
      "server":       { "type": "string" },
      "tool":         { "type": "string" },
      "arguments":    { "type": "object" },
      "redirect_url": { "type": "string" }
    },
    "required": ["action"]
  }
}
```

Per-action contract:

| action | required fields | returns |
|---|---|---|
| `help` | — | usage doc string |
| `list_servers` | — | `[{ name, status, transport, tools_count }]` |
| `list_tools` | `server` | `[{ name, description }]` |
| `describe_tool` | `server`, `tool` | `{ name, description, input_schema }` |
| `call` | `server`, `tool`, `arguments` | tool's `CallToolResult` |
| `status` | `server?` | per-server health / last error / OAuth state |
| `login` | `server` | `{ flow: "device", user_code, verification_url, ... }` or `{ flow: "paste", authorize_url, state, ... }` — see §6.4 |
| `complete_login` | `server`, `redirect_url` | `{ ok: true }` or `{ error }` — paste flow only; device flow polls internally |

### 5.3 Agent loop interaction

Typical multi-turn usage (lazy connect at first need, idle eviction after TTL):

- **Turn 1** — LLM calls `mcp(action: "list_servers")`; no IO, served from config cache. Returns `["github (stdio)", ...]`.
- **Turn 2** — LLM calls `mcp(action: "list_tools", server: "github")`; `lazy_connect("github")` spawns child proc, `peer.list_all_tools()` fetches descriptors. Returns `[{name, description}, ...]`.
- **Turn 3** — LLM calls `mcp(action: "call", server, tool, arguments)`; `peer.call_tool()` invokes. Returns `CallToolResult`.
- **Idle (no MCP call for `idle_ttl`)** — the background eviction loop shuts down the child proc and drops the Peer (only if not mid-call); config + descriptor cache retained for fast re-connect.

### 5.4 Module layout

```
openab-agent/src/
├── agent.rs           (existing — add 1 match arm in execute_tool)
├── auth.rs            (existing — TokenStore reused by mcp/oauth.rs)
├── llm.rs             (existing — UNCHANGED, ToolDef is already generic)
├── tools.rs           (existing — add `mcp` to tool_definitions())
├── skills.rs          (existing — UNCHANGED)
└── mcp/               (NEW module)
    ├── mod.rs         (public: McpRuntimeManager, dispatch())
    ├── config.rs      (mcpServers schema, ${env:VAR} interpolation)
    ├── runtime.rs     (per-server lifecycle, lazy connect, idle TTL)
    ├── meta_tool.rs   (action dispatch: list_servers / list_tools / ...)
    ├── oauth.rs       (uses src/auth.rs TokenStore; built-in providers)
    └── breaker.rs     (circuit breaker per server)
```

Estimated total: **500-750 LOC** (no `reload.rs`; per-session refresh handled by `McpRuntimeManager::new()` re-reading config at session start). `llm.rs` is unchanged because both Anthropic and OpenAI Responses providers consume the generic `ToolDef` abstraction.

#### 5.4.1 Runtime activation & isolation choices

Three intentional choices that surfaced in PR #959 review (chaodu F2 / F6 / F7) and are load-bearing enough to belong in the design contract:

1. **Runtime activation = config presence (F6, always-on, no env switch).** `load_runtime_or_warn()` returns `Some(manager)` whenever `mcp.json` declares ≥1 server, and `None` otherwise. MCP is a first-class, always-supported capability — there is no separate enable flag; declaring a server *is* the activation signal. (An earlier draft gated activation behind an `OPENAB_AGENT_MCP={1,true,yes,on}` env var so an incidentally-present `mcp.json` couldn't auto-spawn third-party child processes. That gate was dropped: MCP is always on, and the operator owns what ships in `mcp.json`. The stdio child env-scrubbing in choice 2 remains the standing mitigation against an untrusted server reading inherited secrets.) The CLI subcommands (`mcp list / status / connect / doctor`) call `load_config_or_exit` rather than `load_runtime_or_warn`, so they inspect a config without starting the long-running runtime.
2. **Stdio child env scrubbing (F2, intentional security).** `Dial::Stdio` calls `env_clear()` and passes only the 4-var baseline allowlist (`HOME`, `PATH`, `TERM`, `USER` on Unix; Windows equivalents) plus the explicit `env:` map from `mcp.json`. Reasoning: openab-agent inherits high-value secrets from its launcher (`DISCORD_BOT_TOKEN`, `ANTHROPIC_API_KEY`, AWS credentials, GitHub tokens) and stdio MCP servers are third-party binaries with no contractual constraint on what they read from their environment. Leaking those by default is a much larger risk than the convenience of inherited proxy/locale settings. Servers that genuinely need additional env (proxy, certs, locale, provider config) declare them per-server in the config — a future `inherit_env` opt-in list is tracked as follow-up if user demand surfaces.
3. **Per-process shared `McpRuntimeManager` (F7).** A single manager is `Arc`-cloned across all ACP sessions of the same process. Reasoning: MCP servers are expensive to spawn (stdio child fork, HTTP handshake + OAuth) and most are pure-state read-only tools where cross-session visibility is benign. Trade-off: a `mcp connect github` in session A makes the `github` server immediately available in session B. We accept this — per-session isolation would multiply child processes and break the breaker / TTL accounting in §5.7 / §5.9.

#### 5.4.2 Discovery slice — bounded catalogue + idle semantics (F1)

The §5.1 / §5.2 single `mcp` meta-tool minimizes the LLM-facing tool surface, but it also *hides* the configured server names from the LLM. The F1 PoC reproduced the resulting failure mode: when a user said "use mcp fs to list /workspace", the LLM called `mcp(action: "status")`, saw `fs: disconnected`, read it as "broken", and refused to retry. Two intentional choices remove that failure mode without re-flattening the tool surface:

1. **Static server catalogue in the system prompt.** `mcp::format_system_prompt_appendix(manager)` appends a `## MCP tool` section containing the tool intro plus `- **{name}** ({transport})` per configured server (with a `requires \`mcp login <name>\`` annotation when an `oauth` block is present). The list is built once at `Agent::new_boxed` time from a sync `manager.catalog()` snapshot frozen at `from_config`, so no async or lock coordination is needed inside `build_system_prompt`. Token-budget invariance is preserved: section size grows **O(server count)** — not O(server count × tool count) — because per-tool descriptors stay behind `mcp(action: "list_tools", server)`. The PoC measured ≤100 tokens per server-side entry under this pattern; flattening tools per-server (≈ what the multi-tool alternative in §4.1 would expose) blows that budget by ~30× for a typical 30-tool github server.

   Mirror with the Skills catalogue (`skills::format_skills_prompt`): both advertise *names + headline metadata* in the always-present system prompt and force *body / contract* discovery through an explicit tool call (`mcp(action: "list_tools" | "describe_tool")` here, `read("skills/<name>")` there). Same intent (the LLM knows the surface exists; details are lazy), same token-budget shape (linear in surface count, not in surface depth).

2. **Status label `idle` for lazy-connect servers.** The meta-tool's `status_label` returns `idle` — not `disconnected` — when a server is in `ServerStatus::Disconnected` with no failure history. `disconnected` reads as "broken" to the LLM (PoC observation above); `idle` correctly signals "ready, will dial on first call". The genuine failure case still maps to `status: "failed"` with the dial / handshake error in `last_error`, so the LLM can distinguish "not tried yet" from "tried and broke". The system-prompt section also advertises these semantics explicitly so the LLM doesn't have to guess.

These choices are wired into PR #959 (Phase 1) because the failure mode they fix is reachable as soon as `list_servers` and `status` ship — deferring to Phase 2/3 would mean shipping a known-broken discovery UX on the foundation slice.

### 5.5 `rmcp` dependency & features

```toml
# openab-agent/Cargo.toml
[dependencies]
rmcp = { version = "1.7", default-features = false, features = [
    "client",
    "transport-child-process",
    "transport-streamable-http-client-reqwest",
    "auth",
] }
```

- `client` only — we host nothing
- `transport-child-process` — stdio servers (majority of registry, see §3.8)
- `transport-streamable-http-client-reqwest` — vendor-hosted SaaS endpoints (reqwest is already a transitive dep)
- `auth` — OAuth helpers
- `default-features = false` — avoid pulling SSE / server features we don't need (SSE intentionally omitted per §3.8 — superseded by Streamable HTTP in MCP spec 2025-11-25, all surveyed vendors migrated or migrating)

Binary delta estimate: **+1-2 MB** (see §7).

### 5.6 Config schema

Single root key `mcpServers` to match Claude Code / Codex / Cursor / Cline convention. Loaded from `.openab/agent/mcp.json` (project) and `~/.openab/agent/mcp.json` (global), project-local takes precedence on name collision.

```jsonc
{
  "mcpServers": {
    "github": {
      "type": "stdio",
      "command": "github-mcp-server",
      "args": ["--repo-token", "${env:GITHUB_TOKEN}"],
      "env": { "GH_HOST": "github.com" }
    },
    "linear": {
      "type": "http",
      "url": "https://mcp.linear.app/mcp",
      "oauth": { "provider": "linear" }
    },
    "fs": {
      "type": "stdio",
      "command": "mcp-server-filesystem",
      "args": ["/workspace"],
      "tool_filter": { "include": ["read_*", "list_*"] }
    }
  }
}
```

- `${env:VAR}` interpolation matches Cursor / Cline; missing var = startup error for that server (others continue)
- `tool_filter` supports `include` / `exclude` glob lists (lifted from OpenHands' `filter_tools_regex`)
- Per-server failure isolated — one bad server does not block agent boot

### 5.7 Lifecycle

```
                    ┌─────────────────────────────────────┐
                    │ McpRuntimeManager  (1 per agent)    │
                    │                                     │
                    │  config:           Arc<McpConfig>   │
                    │  servers:          Map<name, Hdl>   │
                    │  idle_ttl:         Duration (10m)   │
                    │  max_concurrent:   usize    (10)    │
                    └─────────────────────────────────────┘
                                    │
                                    │ on first call needing server X:
                                    ▼
                    ┌─────────────────────────────────────┐
                    │ ServerHandle (lazy)                 │
                    │                                     │
                    │  state: Disconnected | Connecting | │
                    │         Connected(Peer) | Failed |  │
                    │         NeedsAuth                   │
                    │  last_used: Instant                 │
                    │  breaker: CircuitBreaker            │
                    │  tools_cache: Vec<ToolDef>          │
                    └─────────────────────────────────────┘
                                    │
                  ┌─────────────────┼─────────────────┐
                  │                                   │
            ┌───────────┐                       ┌───────────┐
            │ child proc│                       │ HTTP conn │
            │ (stdio)   │                       │ (reqwest) │
            └───────────┘                       └───────────┘
```

- **Lazy connect**: server is `Disconnected` at boot; transitions to `Connecting → Connected` on first action needing it
- **Idle eviction**: a background eviction loop (started at agent run) evicts servers idle > `idle_ttl` (default 10m, configurable) — but only when the server is not mid-call (`in_flight == 0`); a busy server is never torn out from under an in-flight call. State drops to `Disconnected` (surfaced to the LLM as `idle`); tools cache retained for fast re-connect
- **Concurrency cap**: `max_concurrent_servers` bounds simultaneously-`Connected` servers (default 10; see §7 for constrained-env tuning). When connecting a new server would exceed the cap, the LRU **idle** (`in_flight == 0`) connected server is evicted first; if every connected server is busy, the cap is exceeded transiently rather than evicting an in-use server
- **Connection reuse**: while connected, all `mcp call` actions reuse the same `Peer`

### 5.8 Config refresh model

Rather than file-watching mid-session, openab-agent re-reads `mcp.json` at session boundaries:

- **New ACP session** → `McpRuntimeManager::new()` parses `mcp.json` from scratch; ~5 LOC of glue, zero hot-path code
- **Mid-session config edit** → not visible until next session; users re-open the Discord/Slack thread (cheap in openab's per-thread session model)
- **Process restart** → applies config changes globally; rarely needed because broker spawns short-lived agent processes per session

This drops `notify` crate + lease counter + diff applier (~150 LOC, race-condition hotspot) for an 80% UX coverage. Hermes' `/reload-mcp` slash command (§3.1) is the precedent for "explicit user-triggered reload >> implicit file watcher" in a coding-agent context.

### 5.9 Error isolation & circuit breaker

Adopted from Hermes Agent (the only surveyed project that ships one):

```
                  ┌─────────────────────────────────────────┐
                  │ CircuitBreaker (per server)             │
                  │                                         │
                  │  state: Closed | Open | HalfOpen        │
                  │  fail_threshold: 3 (configurable)       │
                  │  cooldown: 60s (configurable)           │
                  └─────────────────────────────────────────┘
                                    │
            ┌───────────────────────┼───────────────────────┐
            │                       │                       │
            ▼                       ▼                       ▼
       3 consecutive fails     60s elapsed             1 success
       ─────────────►         ─────────────►          ─────────────►
       Closed → Open          Open → HalfOpen         HalfOpen → Closed
                              (allow 1 probe)
                                                            │
                                                            │ probe fails
                                                            ▼
                                                       HalfOpen → Open
                                                       (reset cooldown)
```

While `Open`, `mcp call` returns `{"error":"server unavailable, cooldown 45s remaining"}` immediately — no child-process resurrection attempts, no LLM hang.

`rmcp` error model maps cleanly:

| `rmcp` error | meta-tool response | Counts toward breaker? |
|---|---|---|
| `ServiceError::McpError` (protocol) | `{ error: msg, code }` | No (server-level intent) |
| `ServiceError::TransportSend/Closed` | `{ error: "transport", server: ... }` | Yes |
| `CallToolResult { isError: true }` | passed through as result | No (tool-level) |

---

## 6. OAuth

### 6.1 Shared TokenStore

`openab-agent/src/auth.rs` already implements hand-rolled PKCE for Codex (`CODEX_AUTHORIZE_URL`, port 1455). The TokenStore (`~/.openab/agent/auth.json`, 0o600) is reused — `mcp/oauth.rs` calls into the same store with namespaced keys (`mcp:<server_name>` vs `codex`).

**Persistence assumption**: TokenStore is treated as persistent state. Deployments must mount `~/.openab/` on durable storage — hostPath / PVC (k8s work-agents), volume + S3 sync (Fargate Mira), or developer-laptop home directory. Ephemeral container filesystems force a re-bootstrap on every restart and are not a supported configuration.

**Cold-start refresh**: on process start the runtime reads TokenStore lazily (on first `mcp call` per server). Expired access tokens trigger an in-process refresh via the stored refresh token; success updates the store and proceeds transparently. Refresh failure (revoked / expired refresh token) flips the server's state to `NeedsAuth` (§5.7); the next `mcp call` returns an error that prompts the LLM to re-run the §6.4 login flow. No human interaction is required as long as the refresh token remains valid.

**Refresh-token rotation race with async persistence layers**: OAuth 2.1 servers issue a new refresh token on every rotation and immediately revoke the previous one; reuse of a revoked refresh token is treated as a replay attack and cascade-revokes the entire token chain. Deployments where TokenStore persistence is asynchronous (Fargate S3 sidecar sync, eventually-consistent volumes) must flush new tokens to durable storage *before* the agent can be killed — otherwise a Spot interruption between local write and remote sync restores the revoked token from S3 on the next task and locks the user out. Contract:

- **Agent side**: `TokenStore` calls `fsync(2)` after every write to `auth.json`
- **Deployment side**: the S3 / volume sync layer must trigger on `auth.json` mtime change (`inotify` / `fsnotify` event), not poll on a cron. Cron-driven sync (≥1 min interval) is incompatible with refresh-token rotation under Spot interruption
- **Reference deployment**: Mira (openab-ecs Fargate Spot) `mira-home/` S3 sync configuration

### 6.2 Built-in providers (Phase 2)

| Provider | Auth URL | Token URL | Callback | Scopes |
|---|---|---|---|---|
| `anthropic-mcp` | `https://claude.ai/oauth/authorize` | `https://platform.claude.com/v1/oauth/token` | `localhost:53692/callback` | `org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload` (subset varies per use) |
| `github-copilot` | (existing pi/anthropic flow) | existing | existing | existing |
| `generic` | from `mcpServers[name].oauth.authorize_url` | from `.oauth.token_url` | dynamically allocated port | from `.oauth.scopes` |

Callback values apply when the browser flow is engaged (`--browser` / `$DISPLAY` set), and when the agent-guided paste-back branch of §6.4 is selected (user copies the redirect URL from the browser URL bar). The device-code branch of §6.4 ignores the callback entirely.

### 6.3 Custom provider extension point

Config can declare `oauth: { authorize_url, token_url, client_id, scopes, device_authorization_endpoint?, redirect_uri?, discovery?, discovery_allowlist? }` for any server. The generic provider handles PKCE + callback + token persistence. No code change needed for new MCP servers that use standard OAuth 2.1. If `device_authorization_endpoint` is set, §6.4 device-code flow is preferred over paste-back. RFC 8414 dynamic discovery is opt-in only and requires an allowlist — see §6.4.

`oauth.redirect_uri` is required by the paste-back branch of §6.4 for custom providers — it must match the URL pre-registered with the provider's OAuth app, since custom paste-back doesn't bind a local listener (built-ins pin their callback in `ProviderSpec`; the device-code branch ignores it).

### 6.4 Agent-guided OAuth flow (default)

openab-agent's primary deployment surface is containerized (k8s pods, Fargate tasks) where `localhost:53692/callback` is unreachable and there is no display to open. Two non-browser flows are supported; the runtime picks per server based on capability. Browser-callback remains a laptop-only opt-in (`$DISPLAY` set, or `--browser` passed to `openab-agent mcp login`).

**Selection logic** (on `mcp(action: "login", server: X)`):

1. If `X` declares an `oauth.device_authorization_endpoint` in config (§6.3), runtime uses **device-code flow** (RFC 8628). Matches openab's existing CLI convention (`claude auth login`, `codex --device-auth`, `grok --device-auth`).
2. Else runtime uses **paste-back flow** (standard auth-code + PKCE). Universal fallback for OAuth 2.1 servers without a device endpoint (Linear, Notion, Figma, Sentry, ...).

RFC 8414 dynamic discovery (`/.well-known/oauth-authorization-server`) is **disabled by default**. Operators opt in per-server via `oauth.discovery: true` plus an explicit `oauth.discovery_allowlist` of permitted domains (e.g. `["*.anthropic.com"]`); boot rejects `discovery: true` without an allowlist. Rationale: awsvpc egress restrictions + SSRF surface in multi-tenant deployments.

**Device-code flow** (typically platform OAuth: Anthropic, OpenAI, xAI):

- `login` returns `{ flow: "device", user_code, verification_url, expires_in }`. Agent relays to chat: "Open `https://example.com/device`, enter code: `ABCD-EFGH`".
- Runtime polls the token endpoint in background (5s interval, RFC 8628 §3.5). On success, persists tokens under `mcp:X`, transitions server to `Disconnected` so the next `connect()` reads the cached token via the oauth-aware dial path and reaches `Connected` through the normal lifecycle. Keeping the rmcp handshake out of the polling task avoids spawning child processes from a detached `tokio::task`.
- LLM checks `mcp(action: "status", server: X)` to learn when the polling loop completes (status leaves `Connecting`); `complete_login` is not required for this branch — the next `mcp call` triggers `connect()`.

**Paste-back flow** (typically MCP SaaS: Linear, Notion, Figma, ...):

- `login` returns `{ flow: "paste", authorize_url, state }`. Runtime persists transient `{verifier, state}` in TokenStore. Agent relays to chat: "Open this link, sign in, paste the URL you land on back here".
- User pastes the URL as next chat message; LLM calls `mcp(action: "complete_login", server: X, redirect_url: "...")`.
- Runtime parses `code` + `state`, validates `state`, performs PKCE token exchange against `token_url`, persists tokens under `mcp:X`, drops transient state.

**Security** (both flows):

- Device-code `user_code` is short-lived (RFC 8628 §3.2, typically ≤10 min); an attacker who sees the code in chat must also race the polling loop and prove device ownership.
- Paste-back redirect URL carries only the authorization code (OAuth 2.1 PKCE; implicit/hybrid removed); code is single-use + ≤10 min; PKCE verifier held in-process makes intercepted codes unusable.
- Token exchange happens entirely inside the agent process; the chat channel never carries access or refresh tokens. Refresh rotation runs in-process per §6.1.

`openab-agent/src/auth.rs` already ships all three paths for Codex OAuth (browser L150-244, paste-back L165-201, device L328-440). This ADR generalizes that pattern across MCP servers and centralizes flow selection on per-server capability rather than per-CLI hard-coding. OpenHands notes the same headless-OAuth incompatibility (§3.5) without shipping a fix.

---

## 7. Memory Impact Analysis

Included because the sidecar alternative (§4.1 B) was motivated by memory.

`openab-agent` baseline is 15-40 MB RSS. `rmcp` with the §5.5 feature set adds +1-2 MB binary delta and +0 MB idle RSS (no servers configured). Once servers connect, child processes dominate: Go ~10-20 MB, Rust ~5-10 MB, Python/Node ~30-80 MB each.

| Aspect | A. Naïve flat | B. Sidecar | **C. In-core + meta-tool** |
|---|---|---|---|
| Idle RAM delta | +1-2 MB | +0 MB | +1-2 MB |
| Per-server RAM | +5-80 MB (child) | +15 MB bridge + 5-80 MB | +5-80 MB |
| System prompt tokens | +17,000 | +600 (if sidecar discloses lazily) | +600 |
| Lifecycle complexity | Medium | High (2 procs, IPC, version skew) | Medium |
| Crash blast radius | Bad server kills loop | Bridge crash = all gone | Bad server isolated |

The 1-2 MB sidecar saving is dominated by per-server child RAM (identical across architectures) and by token cost (identical *as long as progressive disclosure is used*). Memory does not justify the sidecar.

**Constrained-environment note (Fargate / small Kubernetes pods).** Fargate Spot tasks at 512 MB / 1 GB have no swap; OOMKill is hard. Worst-case stack — agent baseline 40 MB + 5 Node/Python stdio servers at 80 MB each + LLM context buffers — sums to ~440-540 MB, which trips a 512 MB task before any prompt processing. Two mitigations: (a) lower `max_concurrent_servers` to 3 in `mcp.json` (§5.7), bounding worst case to ~280 MB; (b) prefer Go/Rust stdio servers (5-20 MB) or HTTP servers (0 MB local) over Node/Python interpreters. The `mcp doctor` CLI (§8) flags configurations whose worst-case sum exceeds the cgroup limit.

---

## 8. CLI Surface

```
openab-agent mcp list                       — show configured servers + status
openab-agent mcp status [server]            — health, last error, OAuth state
openab-agent mcp add <name> <command>       — append a stdio server to config
openab-agent mcp add <name> --url <url>     — append an http server
openab-agent mcp remove <name>              — remove a server from config
openab-agent mcp login <name> [--browser]   — run OAuth flow (see §6.4; --browser opts into localhost callback)
openab-agent mcp refresh <name>             — force-refresh OAuth token
openab-agent mcp test <name> <tool> [json]  — invoke a tool from CLI (debug)
openab-agent mcp doctor                     — diagnose config, network, auth
```

Subcommand placement under existing `openab-agent` binary — no new binary. CLI is a thin wrapper over `McpRuntimeManager` to keep the same code path validated by both LLM-driven and human-driven flows.

---

## 9. Rollout Plan

Delivered across three phases (all landed):

1. **Foundation** — `rmcp` + stdio + meta-tool + minimal CLI
2. **Network & auth** — Streamable HTTP transport + OAuth providers + `login`/`refresh` CLI
3. **Resilience** — per-server circuit breaker (§5.9), idle eviction + concurrency cap (§5.7), and `doctor` CLI

The runtime's activation contract is described in §5.4.1 (the long-running ACP path calls `load_runtime_or_warn()`, the CLI subcommands use `load_config_or_exit`); there is no Cargo `--features mcp` build flag.

---

## 10. Open Questions

1. **Should `mcp.json` live in the agent or the broker?** Agent owns its own config today; broker's `[agent].inherit_cloud_mcp_servers` (issue #753) is a separate concern. Proposal: agent reads `mcp.json` directly; broker can layer additional servers via env or kubectl ConfigMap. **Owner**: needs broker-team alignment.
2. **Native-agent feature parity with broker-forward path.** PRs #329/#330/#345/#903 attempted broker-side MCP forwarding to backing CLIs. With native MCP in openab-agent, do we deprecate that path, keep it for non-native CLIs, or unify? Proposal: native agent uses its own MCP runtime; broker continues to forward to backing CLIs that lack native MCP (Cursor, Copilot). **Owner**: broker-team.

Resolved at design time (tracked in tracking issue, not open): tool-naming prefix (`<server>_<tool>` single-underscore, matching Hermes §3.1 / opencode §3.2 convention), `session/load` re-enumeration (process-local state, re-read), per-tool permission gates (post-Phase-3 opt-in flag), `resources`/`prompts` capabilities (v2).

---

## 11. References

### Internal

- `docs/adr/openab-agent.md` — agent charter, design principles cited in §4.2
- `docs/adr/pr-contribution-guidelines.md` — prior-art requirements followed in §3
- `openab-agent/src/skills.rs` (PR #955) — extension-pattern precedent cited in §4.3
- `openab-agent/src/auth.rs` — TokenStore reused in §6.1
- PRs #329, #330, #345, #903 — closed broker-forward attempts, §1.3
- Issue #753 — broker-side MCP opt-out (out of scope)
- PR #951 — SessionPool persisted-mapping fix (informs §10 resolved-at-design-time list)

### External — projects

- Hermes Agent: https://github.com/NousResearch/hermes-agent
- opencode: https://github.com/anomalyco/opencode (formerly https://github.com/sst/opencode)
- pi-mcp-adapter: https://github.com/nicobailon/pi-mcp-adapter
- Goose: https://github.com/aaif-goose/goose (formerly https://github.com/block/goose)
- OpenHands: https://github.com/OpenHands/OpenHands
- rmcp: https://github.com/modelcontextprotocol/rust-sdk
- OpenClaw (evaluated per `pr-contribution-guidelines.md`, scope not applicable — see §3; canonical repo URL not publicly resolvable, internal reference via avasdream blog cited in guidelines)

### External — specs & research

- MCP spec: https://modelcontextprotocol.io
- MCP spec changelog 2025-11-25 (Streamable HTTP supersedes HTTP+SSE): https://modelcontextprotocol.io/specification/2025-11-25/basic/transports
- MCP SEP-1576 — Mitigating Token Bloat in MCP: https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1576
- Atlassian Rovo MCP SSE→Streamable HTTP migration notice (sunset 2026-06-30): https://community.atlassian.com/forums/Rovo-articles/Migrating-from-Atlassian-s-MCP-Server-SSE-to-Streamable-HTTP/ba-p/3092878
- Figma MCP server (Streamable HTTP): https://help.figma.com/hc/en-us/articles/32132100833559-Guide-to-the-Dev-Mode-MCP-Server
- Anthropic — Equipping agents for the real world with Agent Skills: https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills
- Anthropic — Code execution with MCP: https://www.anthropic.com/engineering/code-execution-with-mcp
- Simon Willison — Claude Skills (2025-10-16): https://simonwillison.net/2025/Oct/16/claude-skills/
- StackOne — MCP Token Optimization: https://www.stackone.com/blog/mcp-token-optimization/
- opencode issues cited in §3.2, §4.1, §7: #11868, #7261, #13041

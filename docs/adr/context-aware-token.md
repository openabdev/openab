# ADR: Context-Aware Token for Agent-Initiated Platform Operations

- **Status:** Proposed
- **Date:** 2026-05-03
- **Author:** @chaodu-agent
- **Related:** #339, PR #527 (superseded)

---

## 1. Context & Problem

OAB agents today are **passive receivers** — they get a prompt from the adapter and return a response. But real-world usage reveals scenarios where agents need to **actively interact** with the platform:

| Scenario | Current State | Desired State |
|---|---|---|
| Update thread title to reflect task status | Agent uses `curl` via steering doc hack | Agent calls Discord API directly |
| Fetch a specific historical message | ❌ Not possible — agent only sees conversation window | Agent fetches any message by ID |
| Notify a bot in another channel | ❌ Not possible — agent is confined to current channel | Agent sends cross-channel messages |
| Ping another bot to trigger a reaction | ❌ Not possible | Agent mentions bot in target channel |

### Why Not PR #527's Approach?

PR #527 proposed always prepending quoted message content to the agent prompt at the OAB transport layer. While well-implemented, this approach:

- **Always pays the cost** (~500 tokens per reply) even when the agent already has the context from conversation history
- **Only solves one edge case** (reply/quote context) out of the broader set of agent-initiated operations
- **Puts the decision in the wrong layer** — OAB (transport) decides what context the agent needs, instead of the agent deciding for itself

A context-aware token lets the agent **pull context on demand** — only when it determines the context is needed.

---

## 2. Prior Art & Industry Research

### OpenClaw

**How it works:** OpenClaw uses a **mediated architecture** — agents never get direct API tokens or raw platform access. All platform interactions flow through the Gateway + Channel Adapter layer:

```
Agent → Gateway (tool call / response) → Channel Outbound Adapter → Platform API
Platform API → Channel Monitor → Gateway (normalized MsgContext) → Agent
```

Key mechanisms:

- **50-action message action system** — agents invoke named actions (`send`, `thread-create`, `read`, `edit`, `delete`, `search`, `pin`, `role-add`, etc.) dispatched to channel plugins. Each action is gated by `supportsAction` checks against a `ChannelCapabilities` object declaring what each platform supports.
- **Session tools for cross-channel operations** — `sessions_list`, `sessions_history`, `sessions_send`, `sessions_spawn` let agents discover sessions, fetch transcripts, send messages across channels, and spawn sub-agents. Inter-session messages are tagged with `message.provenance.kind = "inter_session"` for auditability.
- **Layered policy model** — security enforced at multiple levels: Tool Profile → Provider Profile → Global Policy → Provider Policy → Agent Policy → Group Policy → Sandbox Policy. Later sources override earlier ones.
- **Send Policy** — configurable deny/allow rules for agent-initiated messages by channel and chat type, enforced at the gateway level.
- **Session-based trust boundaries** — `main` sessions (operator) get full host access; `dm` and `group` sessions are sandboxed in Docker containers by default.

**Security lessons from advisories:**

| Advisory | Risk | Lesson for OpenAB |
|---|---|---|
| [GHSA-v3qc-wrwx-j3pw](https://github.com/openclaw/openclaw/security/advisories/GHSA-v3qc-wrwx-j3pw) (High) | LLM agent disabled exec approval via config.patch | Behavioral constraints alone are insufficient — agents can modify their own config to bypass them |
| [GHSA-2rqg-gjgv-84jm](https://github.com/openclaw/openclaw/security/advisories/GHSA-2rqg-gjgv-84jm) (High, CVSS 8.8) | Workspace boundary override via attacker-controlled params | Gateway must enforce boundaries regardless of caller overrides |
| [GHSA-7jx5-9fjg-hp4m](https://github.com/openclaw/openclaw/security/advisories/GHSA-7jx5-9fjg-hp4m) (Moderate) | ACP permission auto-approval bypass via untrusted metadata | Auto-approval heuristics trusting untrusted metadata are dangerous |

**Key insight:** The mediated architecture IS the security boundary. Agents never touch platform APIs directly — the Gateway enforces what's allowed.

References:
- Channel plugin types: [`src/channels/plugins/types.plugin.ts`](https://github.com/openclaw/openclaw)
- Session tools: [docs/concepts/session-tool](https://molty.finna.ai/docs/concepts/session-tool)
- Security advisories: [github.com/openclaw/openclaw/security](https://github.com/openclaw/openclaw/security)

### Hermes Agent

**How it works:** Hermes Agent uses a **tool-based architecture** — platform operations are exposed as LLM tools with OpenAI function-calling schemas. The agent decides when to call them.

Key mechanisms:

- **Discord tool** ([`tools/discord_tool.py`](https://github.com/NousResearch/hermes-agent)) — 14 actions split into two toolsets: `discord` (core: `fetch_messages`, `search_members`, `create_thread`) and `discord_admin` (server management). Uses Discord REST API directly with the bot token.
- **Cross-platform `send_message` tool** ([`tools/send_message_tool.py`](https://github.com/NousResearch/hermes-agent)) — supports 17+ platforms (Telegram, Discord, Slack, WhatsApp, Signal, Matrix, etc.) with human-friendly name resolution via `gateway/channel_directory.py`.
- **Dynamic schema based on capabilities** — Discord tool detects bot intents via `GET /applications/@me` and hides unavailable actions from the schema (e.g., `search_members` hidden if GUILD_MEMBERS intent is missing). This prevents the LLM from hallucinating calls to tools it can't use.
- **Config-based action allowlist** — `discord.server_actions` in config.yaml restricts which actions the agent can perform. Runtime re-checks the allowlist even if a stale cached schema exposes a disabled action.
- **Defense-in-depth** — three layers: (1) schema filtering removes unavailable actions, (2) runtime allowlist check at dispatch, (3) platform-level permission errors (Discord 403) handled gracefully with actionable guidance.
- **Error redaction** — `_sanitize_error_text()` strips secrets (GitHub PATs, Bearer tokens, API keys) from error messages before they reach the LLM.

**Gaps relevant to OpenAB:**
- Only Discord has a comprehensive read tool (`fetch_messages`). Other platforms (Telegram, Slack, etc.) have no equivalent — agents can send to them but not read from them.
- No formal capability/scope system beyond the Discord config allowlist. The agent either has the tool or doesn't.

**Key insight:** Tools provide a structured, auditable interface vs raw API access. The schema itself acts as a capability declaration — the agent can only call what's in the schema.

References:
- Discord tool: [`tools/discord_tool.py`](https://github.com/NousResearch/hermes-agent)
- Send message tool: [`tools/send_message_tool.py`](https://github.com/NousResearch/hermes-agent)
- Tool registry: [`tools/registry.py`](https://github.com/NousResearch/hermes-agent)
- Security docs: [hermes-agent.nousresearch.com/docs/user-guide/security](https://hermes-agent.nousresearch.com/docs/user-guide/security)

### Comparison

| Aspect | OpenClaw | Hermes Agent | OpenAB (this ADR) |
|---|---|---|---|
| Agent-to-platform interface | Mediated — Gateway dispatches named actions | Tool-based — LLM tools with function-calling schemas | Direct — agent calls platform API with token |
| Security enforcement | Technical — Gateway enforces layered policy | Technical — schema gating + runtime allowlist + platform permissions | Behavioral — steering docs define allow/deny (see §5 for evolution path) |
| Cross-channel operations | `sessions_send` with provenance tagging | `send_message` tool with 17+ platform support | Agent uses token + `curl` |
| Message fetching | `sessions_history` + `read` action | `discord_tool.fetch_messages` (Discord only) | Agent calls REST API directly |
| Audit trail | Inter-session provenance tags | Tool call logs | None built-in (see §5 for planned additions) |

### Why OpenAB Diverges

Both OpenClaw and Hermes use **mediated architectures** where the platform (Gateway or tool runtime) controls what agents can do. OpenAB's context-aware token takes a different path: **direct agent access with behavioral constraints**.

This is a deliberate tradeoff:

1. **OAB's architecture** — OAB is a passive transport layer by design. Adding a Gateway mediation layer or tool runtime would be a fundamental architecture change, not an incremental feature.
2. **Agent diversity** — OAB supports 4+ different agent runtimes (Kiro CLI, Claude Code, Codex, Gemini). A mediated approach would require each runtime to integrate with OAB's tool/action system. Direct token access works with any agent that has shell access.
3. **Pragmatism** — The pattern already works in production (超渡法師 uses `curl` + bot token for thread title updates). This ADR formalizes and hardens what's already happening.

The security gap is real and acknowledged — §5 below describes the evolution path from behavioral-only to defense-in-depth.

---

## 3. Proposed Design

### Core Concept

Give the agent a **scoped platform token** (e.g., `DISCORD_CONTEXT_TOKEN`) that it can use to perform platform API calls when it judges them necessary. The token is configured by the user in their steering/tools definition, not by OAB core.

```
OAB Layer (transport)              Agent Layer (intelligence)
─────────────────────              ────────────────────────
BOT_TOKEN                          DISCORD_CONTEXT_TOKEN
Passive: receive msg, send reply   Active: fetch, notify, update
OAB doesn't change                 User defines allowed operations
Adapter responsibility             Agent autonomy
```

### How It Works

1. User sets `DISCORD_CONTEXT_TOKEN` in the agent's environment (same bot token or a separate scoped token)

> ⚠️ **Security note:** Using the same bot token as `DISCORD_CONTEXT_TOKEN` grants the agent full bot permissions — this is a convenience shortcut suitable only for trusted, single-operator deployments. For production or multi-tenant environments, use a separate token with minimal scopes when the platform supports it. See §5 for the security evolution path.
2. User defines allowed operations in `tools.md` or steering docs
3. Agent decides at runtime when to use the token — e.g., "user said 'why?' and I'm not sure what they're referring to, let me fetch the referenced message"
4. OAB core is unaware of this — it's purely an agent-side capability

### Scope Definition (User-Controlled)

The trust boundary is initially defined by the user in steering docs. **This is a documentation convention, not a security boundary** — the agent has the full token and is behaviorally constrained, not technically restricted. See §5 for the evolution path toward technical enforcement.

```markdown
# Discord Context Tools

You have DISCORD_CONTEXT_TOKEN for platform operations.

## Allowed
- Update current thread title
- Fetch messages in current channel/thread
- Send messages to specified channels (cross-channel notify)
- Add reactions

## Not Allowed
- Delete messages
- Modify server settings
- Manage roles/permissions
- Create/delete channels
```

---

## 4. Use Cases

### 4a. Smart Quote Resolution (Replaces PR #527)

Instead of always prepending quoted content:

```
User replies to a message: "why?"
  │
  ├─ Agent sees "why?" in prompt
  ├─ Agent checks conversation history — enough context? → respond directly
  ├─ Not enough context? → use token to fetch referenced message
  └─ Now respond with full understanding
```

**Benefit:** Zero extra tokens when context is already available. Only fetches when genuinely needed.

### 4b. Cross-Channel Bot Coordination

```
User: "ask 普渡法師 in #claude-room to review this code"
  │
  ├─ Agent uses token to send message to #claude-room
  ├─ Message mentions 普渡法師 bot
  └─ 普渡法師 receives the message and starts working
```

### 4c. Thread Title Management

```
Agent finishes reviewing PR #527
  │
  ├─ Agent uses token to update thread title
  └─ "🔢 PR #527 reviewed"
```

This is already happening today via steering doc + `curl`. The token formalizes it.

### 4d. Historical Context Retrieval

```
User: "what did Jack say about this yesterday?"
  │
  ├─ Agent searches conversation history — not in window
  ├─ Agent uses token to fetch recent messages from channel
  └─ Finds Jack's message and responds
```

---

## 5. Security Model

### Current State: Behavioral Constraints Only

| Concern | Mitigation | Limitation |
|---|---|---|
| Token is same as BOT_TOKEN — full permissions | Steering docs define allow/deny list | Agent can ignore steering docs (prompt injection, hallucination) |
| Agent could misuse token | Steering docs define explicit scope | No technical enforcement — relies on agent compliance |
| Token leaked in logs | Agent instructed to reference by env var name | No redaction layer — agent could still echo the value |
| Cross-channel abuse | Steering docs restrict target channels | No runtime validation of target channels |

**This is insufficient for production use.** OpenClaw's security advisories demonstrate that behavioral constraints alone fail — their [GHSA-v3qc-wrwx-j3pw](https://github.com/openclaw/openclaw/security/advisories/GHSA-v3qc-wrwx-j3pw) showed an LLM agent disabling its own exec approval via config modification.

### Evolution Path: Behavioral → Defense-in-Depth

The security model evolves across four maturity levels (distinct from the [Rollout Plan in §10](#10-rollout-plan), which tracks implementation milestones):

**Level 1 (Behavioral only — current):**
- Steering docs define allowed operations
- Suitable for trusted, operator-controlled agents only
- Acceptable risk: operator is the user AND the admin

**Level 2 (Audit logging):**
- OAB logs all outbound HTTP calls from agent processes (network-level observation)
- No enforcement, but provides visibility for post-incident analysis
- Implementation: eBPF-based network monitoring or HTTP proxy with logging

**Level 3 (Proxy enforcement):**
- Agent's `DISCORD_CONTEXT_TOKEN` routes through an OAB-controlled HTTP proxy
- Proxy validates each API call against a configured allowlist:
  ```
  # Example proxy allowlist (config.toml)
  [agent_proxy]
  allowed_endpoints = [
    "GET /channels/*/messages",      # fetch messages
    "PATCH /channels/*",             # update thread title
    "POST /channels/*/messages",     # send messages
    "PUT /channels/*/messages/*/reactions/*",  # add reactions
  ]
  denied_endpoints = [
    "DELETE *",                      # no deletions
    "PUT /guilds/*",                 # no server modifications
  ]
  ```
- Denied calls return 403 with audit log entry
- Inspired by Hermes Agent's defense-in-depth (schema filtering + runtime allowlist + platform permissions)

**Level 4 (True scoped tokens):**
- If Discord (or other platforms) introduce fine-grained token scopes, swap the token
- The agent-side interface doesn't change — only the token's actual permissions narrow

### Comparison with Prior Art

| Layer | OpenClaw | Hermes Agent | OpenAB Level 1 | OpenAB Level 3 |
|---|---|---|---|---|
| Schema/capability gating | ✅ `supportsAction` | ✅ Dynamic schema | ❌ | ❌ |
| Runtime allowlist | ✅ Send Policy | ✅ Config allowlist | ❌ Steering docs only | ✅ Proxy allowlist |
| Platform permission errors | ✅ | ✅ Graceful 403 handling | ✅ (passthrough) | ✅ (passthrough) |
| Audit trail | ✅ Provenance tags | ✅ Tool call logs | ❌ | ✅ Proxy logs |
| Secret redaction | ❌ | ✅ `_sanitize_error_text()` | ❌ | ✅ Proxy strips tokens from errors |

---

## 6. Concrete Resolution Path for Issue #339

Issue [#339](https://github.com/openabdev/openab/issues/339) requests reply/quote context in agent prompts. PR #527 implemented always-on prepending; this ADR supersedes that approach with on-demand fetching.

### How #339 Gets Resolved

There are two distinct scenarios for context resolution. The token is only needed for the second.

**Scenario A — Discord reply (user uses reply/quote feature):**

Discord Gateway already sends `referenced_message` (full message object) and `message_reference` (with `message_id`, `channel_id`, `guild_id`) on reply messages (type 19). OAB's adapter receives this data and can passthrough it to the agent at near-zero cost.

**Step 1 — OAB passthroughs reply metadata (minimal transport-layer change):**

OAB already receives `referenced_message` from Discord's gateway. Instead of prepending the full quoted content (PR #527's approach), OAB injects only the metadata:

```
[Reply context: message_id=1234567890, channel_id=9876543210, author=Jack]
```

This costs ~20 tokens (vs ~500 for full content) and gives the agent enough information to decide whether to fetch.

**Step 2 — Agent decides whether to fetch:**

- If the conversation history already contains the referenced message → respond directly (zero extra cost)
- If the agent needs the full content of the referenced message → use `DISCORD_CONTEXT_TOKEN` to call `GET /channels/{channel_id}/messages/{message_id}` and fetch it
- If no token is configured → agent responds based on available context (graceful degradation)

> **Note:** Because Discord already provides `referenced_message` on reply messages, OAB could alternatively passthrough the full content directly (like PR #527). The metadata-only approach is preferred because it keeps the transport layer minimal and lets the agent decide. Either way, the token is not strictly required for this scenario.

**Scenario B — Non-reply historical lookup (no `referenced_message` available):**

When a user asks about past messages without using Discord's reply feature (e.g., "what did Jack say about this yesterday?"), there is no `referenced_message` in the Gateway event. This is where the context-aware token provides unique value — the agent uses it to call `GET /channels/{channel_id}/messages` and search for relevant messages.

**Step 3 — Steering doc template:**

```markdown
# Reply Context Resolution

When you see `[Reply context: message_id=..., channel_id=..., author=...]`:
1. Check if the referenced message is already in your conversation history
2. If not, and you need it to respond accurately, fetch it:
   curl -s -H "Authorization: Bot $DISCORD_CONTEXT_TOKEN" \
     "https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}"
3. If DISCORD_CONTEXT_TOKEN is not available, respond based on available context

# Historical Context Retrieval (no reply metadata)

When a user references past messages without using Discord reply:
1. Use the token to fetch recent messages from the channel:
   curl -s -H "Authorization: Bot $DISCORD_CONTEXT_TOKEN" \
     "https://discord.com/api/v10/channels/{channel_id}/messages?limit=50"
2. Search the results for relevant context
3. If DISCORD_CONTEXT_TOKEN is not available, ask the user to quote or reply to the specific message
```

### Acceptance Criteria for #339

- [ ] OAB injects reply metadata (`message_id`, `channel_id`, `author`) into agent prompt — small transport-layer PR
- [ ] Steering doc template published for agents to use the token for on-demand fetching
- [ ] At least one agent (超渡法師) validated end-to-end: reply in Discord → agent fetches referenced message → responds with full context
- [ ] Issue #339 closed with reference to this ADR and the implementing PRs

---

## 7. What Changes in OAB?

**Minimal.** The key design principle is preserved — OAB remains a passive transport layer:

- **Reply metadata injection** (for #339): OAB adds `[Reply context: message_id=..., channel_id=..., author=...]` to the prompt when a Discord message has `referenced_message`. This is a small, targeted change in `discord.rs` (~10 lines).
- **Token passthrough**: OAB already passes environment variables to agent processes. No change needed — user adds `DISCORD_CONTEXT_TOKEN` to their agent config.
- **Proxy (Level 3, optional)**: If/when proxy enforcement is added, it would be a new optional component, not a change to OAB core.

---

## 8. Relationship to Existing Features

| Feature | Relationship |
|---|---|
| PR #527 (reply context) | **Superseded** — context-aware token solves the same problem more efficiently (on-demand vs always-on) |
| Custom Gateway ADR | **Complementary** — gateway handles inbound webhooks; context-aware token handles agent-initiated outbound operations |
| Multi-Platform Adapters ADR | **Complementary** — each platform can have its own scoped token type |
| Steering docs | **Extended** — steering docs gain a new responsibility: defining token scope (Level 1 only — Level 3 moves to proxy enforcement) |

---

## 9. Open Questions

| Question | Options | Notes |
|---|---|---|
| ~~One token per platform or unified?~~ | **Decided: Per-platform** — simpler and more secure | Start with Discord, extend later |
| Should OAB inject the token automatically? | No — user configures it in agent env | Keeps OAB uninvolved |
| Rate limiting on agent-initiated calls? | Level 1: rely on platform rate limits; Level 3: proxy-level rate limiting | Proxy can enforce per-agent rate limits |
| How to handle platforms without API tokens? | N/A until needed | LINE, Telegram have different auth models |
| Should OAB provide a proxy from Level 1? | No — start with behavioral constraints for trusted operators, add proxy when multi-tenant or untrusted agents are needed | Complexity should match the threat model |

---

## 10. Rollout Plan

| Phase | Scope | Target | Acceptance Criteria |
|---|---|---|---|
| **Phase 1** | Document the pattern — steering doc template for Discord context token | v0.9.x | Template published, validated with 超渡法師 |
| **Phase 2** | Resolve #339 — OAB injects reply metadata, agent fetches on demand | v0.9.x | #339 closed, end-to-end validation with at least one agent |
| **Phase 3** | Formalize as `tools.md` convention across OpenAB agents | v0.10.x | All 4 agent runtimes have working steering doc templates |
| **Phase 4** | Add optional audit logging for agent-initiated API calls | v0.10.x | Operator can see what API calls agents make |
| **Phase 5** | Evaluate proxy enforcement for multi-tenant / untrusted agent scenarios | v0.11.x+ | Design doc for proxy layer if demand exists |

---

## Consequences

### Positive

- Agent gets platform awareness without OAB core changes
- On-demand context fetching is more token-efficient than always-on prepending
- Enables cross-channel coordination — a capability that was previously impossible
- User controls the scope — no one-size-fits-all behavior imposed by OAB
- Pattern extends naturally to other platforms
- Concrete path to resolve #339 with minimal OAB changes

### Negative

- Phase 1 trust boundary is behavioral (steering docs), not technical — relies on agent compliance. Acceptable for trusted operators; insufficient for multi-tenant deployments.
- Each user must configure the token and define scope — more setup burden
- Agent-initiated API calls add latency when they occur
- No centralized audit of what agents do with the token until Phase 4
- Diverges from industry standard (mediated architecture) — conscious tradeoff for OAB's passive transport philosophy

---

## References

- [Issue #339](https://github.com/openabdev/openab/issues/339) — Original feature request for reply/quote context
- [PR #527](https://github.com/openabdev/openab/pull/527) — Implementation of always-on quote prepending (superseded by this ADR)
- [ADR: Custom Gateway](./custom-gateway.md) — Complementary architecture for inbound webhook handling
- [ADR: Multi-Platform Adapters](./multi-platform-adapters.md) — Platform-agnostic adapter layer
- [OpenClaw Security Advisories](https://github.com/openclaw/openclaw/security) — Real-world security lessons for agent-platform interactions
- [Hermes Agent Tools Runtime](https://hermes-agent.nousresearch.com/docs/developer-guide/tools-runtime) — Tool-based agent interaction architecture

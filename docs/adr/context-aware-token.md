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

## 2. Proposed Design

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
2. User defines allowed operations in `tools.md` or steering docs
3. Agent decides at runtime when to use the token — e.g., "user said 'why?' and I'm not sure what they're referring to, let me fetch the referenced message"
4. OAB core is unaware of this — it's purely an agent-side capability

### Scope Definition (User-Controlled)

The trust boundary is defined by the user in steering docs, not by the token itself (Discord bot tokens don't have fine-grained scopes):

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

## 3. Use Cases

### 3a. Smart Quote Resolution (Replaces PR #527)

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

### 3b. Cross-Channel Bot Coordination

```
User: "ask 普渡法師 in #claude-room to review this code"
  │
  ├─ Agent uses token to send message to #claude-room
  ├─ Message mentions 普渡法師 bot
  └─ 普渡法師 receives the message and starts working
```

### 3c. Thread Title Management

```
Agent finishes reviewing PR #527
  │
  ├─ Agent uses token to update thread title
  └─ "🔢 PR #527 reviewed"
```

This is already happening today via steering doc + `curl`. The token formalizes it.

### 3d. Historical Context Retrieval

```
User: "what did Jack say about this yesterday?"
  │
  ├─ Agent searches conversation history — not in window
  ├─ Agent uses token to fetch recent messages from channel
  └─ Finds Jack's message and responds
```

---

## 4. Security Considerations

| Concern | Mitigation |
|---|---|
| Token is same as BOT_TOKEN — full permissions | Trust boundary enforced by steering docs (agent behavioral constraint) |
| Agent could misuse token | Steering docs define explicit allow/deny list |
| Token leaked in logs | Agent instructed to reference by env var name, never log value |
| Cross-channel abuse | Steering docs restrict which channels agent can target |

### Future: True Scoped Tokens

If Discord (or other platforms) introduce fine-grained token scopes in the future, the architecture is ready — just swap the token. The agent-side interface doesn't change.

---

## 5. What Changes in OAB?

**Nothing.** This is the key design principle:

- OAB core remains a passive transport layer
- The token lives in the agent's environment, configured by the user
- Allowed operations are defined in user steering docs
- OAB doesn't need to know the agent is making platform API calls

This is consistent with OAB's philosophy: OAB handles transport, the agent handles intelligence.

---

## 6. Relationship to Existing Features

| Feature | Relationship |
|---|---|
| PR #527 (reply context) | **Superseded** — context-aware token solves the same problem more efficiently (on-demand vs always-on) |
| Custom Gateway ADR | **Complementary** — gateway handles inbound webhooks; context-aware token handles agent-initiated outbound operations |
| Multi-Platform Adapters ADR | **Complementary** — each platform can have its own scoped token type |
| Steering docs | **Extended** — steering docs gain a new responsibility: defining token scope |

---

## 7. Open Questions

| Question | Options | Notes |
|---|---|---|
| One token per platform or unified? | Per-platform is simpler and more secure | Start with Discord, extend later |
| Should OAB inject the token automatically? | No — user configures it in agent env | Keeps OAB uninvolved |
| Rate limiting on agent-initiated calls? | Rely on platform rate limits + steering doc constraints | Could add agent-side rate limiting later |
| How to handle platforms without API tokens? | N/A until needed | LINE, Telegram have different auth models |

---

## 8. Rollout Plan

| Phase | Scope |
|---|---|
| **Phase 1** | Document the pattern — steering doc template for Discord context token |
| **Phase 2** | Validate with existing agents (超渡法師 already uses `curl` for thread titles) |
| **Phase 3** | Formalize as `tools.md` convention across OpenAB agents |
| **Phase 4** | Evaluate if OAB should provide helper utilities (optional, not required) |

---

## Consequences

### Positive

- Agent gets platform awareness without OAB core changes
- On-demand context fetching is more token-efficient than always-on prepending
- Enables cross-channel coordination — a capability that was previously impossible
- User controls the scope — no one-size-fits-all behavior imposed by OAB
- Pattern extends naturally to other platforms

### Negative

- Trust boundary is behavioral (steering docs), not technical (token scopes) — relies on agent compliance
- Each user must configure the token and define scope — more setup burden
- Agent-initiated API calls add latency when they occur
- No centralized audit of what agents do with the token

---

## References

- [Issue #339](https://github.com/openabdev/openab/issues/339) — Original feature request for reply/quote context
- [PR #527](https://github.com/openabdev/openab/pull/527) — Implementation of always-on quote prepending (superseded by this ADR)
- [ADR: Custom Gateway](./custom-gateway.md) — Complementary architecture for inbound webhook handling
- [ADR: Multi-Platform Adapters](./multi-platform-adapters.md) — Platform-agnostic adapter layer

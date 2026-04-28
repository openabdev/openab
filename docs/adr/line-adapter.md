# ADR: LINE Messaging API Adapter

- **Status:** Accepted
- **Date:** 2026-04-22
- **Last Updated:** 2026-04-28
- **Author:** @chaodu-agent, @iamninihuang

---

## 1. User Story & Requirements

As a LINE user, I want to interact with an OpenAB agent directly in LINE — both in 1:1 DMs and group chats — so that I can use the same AI coding assistant without switching to Discord or Slack.

Requirements:
- Receive user messages from LINE and route them to an agent session
- Send agent responses back to the user via LINE
- Validate webhook signatures to ensure messages are authentically from LINE
- Support user/group allowlists for access control
- Integrate into the existing multi-adapter architecture (run alongside Discord/Slack)

### When to Use LINE

LINE is the right choice when:
- Your users are primarily on LINE (common in Taiwan, Japan, Thailand, Indonesia)
- The primary use case is **1:1 private conversations** with the agent — each user gets a dedicated session, similar to Discord DM
- You need a mobile-first experience — LINE's mobile app is the dominant interface
- You want to reach users who don't have or use Discord/Slack

### When to Use Discord or Slack Instead

LINE is not ideal when:
- **Multiple users need to collaborate with the agent simultaneously** — Discord/Slack threads provide per-conversation isolation; LINE groups share a single session, leading to context pollution
- **You need long, multi-turn conversations in a team setting** — thread-based platforms keep each conversation organized and searchable
- **You have many concurrent users (>10)** — LINE's always-on session model creates higher memory pressure; Discord/Slack's @mention-triggered sessions scale more efficiently
- **You need rich interaction patterns** — Discord/Slack support reactions, file attachments with preview, and threaded discussions; LINE has a 5000-char message limit, no reactions, and no threads

### Summary: Best Fit by Scenario

| Scenario | Recommended Platform | Why |
|---|---|---|
| Individual developer, mobile-first | **LINE** | 1:1 DM works well, convenient on mobile |
| Small team (2-5), async collaboration | **Discord / Slack** | Threads keep conversations organized |
| Large team (10+), concurrent usage | **Discord / Slack** | On-demand sessions scale better |
| Users in LINE-dominant regions | **LINE** (1:1) + **Discord** (team) | Use LINE for personal, Discord for team |
| Public-facing bot for community | **Discord** | Channels + threads + @mention gating |

---

## 2. High-Level Design

### Architecture Overview

```
┌──────────────┐
│ LINE Platform │
└──────┬───────┘
       │ HTTPS POST (webhook)
       ▼
┌──────────────────┐
│ TLS Termination  │  (CDN / Reverse Proxy / Ingress)
└──────┬───────────┘
       │ HTTP
       ▼
┌──────────────────┐
│  Load Balancer   │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐     ┌─────────────────────────────────┐
│  K8s Service     │────▶│  OpenAB Pod                     │
│  (ClusterIP)     │     │                                 │
└──────────────────┘     │  ┌───────────┐  ┌────────────┐  │
                         │  │ Webhook   │  │ Discord    │  │
                         │  │ Handler   │  │ Adapter    │  │
                         │  │ (:8080)   │  │ (WebSocket)│  │
                         │  └─────┬─────┘  └─────┬──────┘  │
                         │        │              │         │
                         │        ▼              ▼         │
                         │  ┌─────────────────────────┐    │
                         │  │    Adapter Router        │    │
                         │  └─────────┬───────────────┘    │
                         │            │                    │
                         │            ▼                    │
                         │  ┌─────────────────────────┐    │
                         │  │    ACP Session Pool      │    │
                         │  │  ┌───────┐ ┌───────┐    │    │
                         │  │  │kiro-cli│ │kiro-cli│...│    │
                         │  │  └───────┘ └───────┘    │    │
                         │  └─────────────────────────┘    │
                         └─────────────────────────────────┘
```

### Message Flow

```
1. LINE user sends message
2. LINE Platform POSTs to webhook endpoint with JSON payload + HMAC signature
3. Webhook handler validates signature using channel secret
4. Handler extracts sender info, message text, and source (user/group/room)
5. Handler determines session key:
   - 1:1 DM  → line:{userId}
   - Group   → line:{groupId}
   - Room    → line:{roomId}
6. Message is routed to AdapterRouter → ACP Session Pool → kiro-cli process
7. Agent response is sent back via LINE Reply API (free) or Push Message API (fallback)
```

### Hybrid Reply/Push Dispatch Flow

```
LINE User                    Gateway                         OAB Core
   │                            │                               │
   │  message + replyToken      │                               │
   │ ─────────────────────────▶ │                               │
   │                            │  1. Verify HMAC signature     │
   │                            │  2. Generate event_id (UUID)  │
   │                            │  3. Cache:                    │
   │                            │     event_id → replyToken     │
   │                            │     (TTL 50s, max 10k)        │
   │                            │                               │
   │                            │  GatewayEvent { event_id }    │
   │                            │ ─────────────────────────────▶│
   │                            │                               │  Store event_id in
   │                            │                               │  ChannelRef.origin_event_id
   │                            │                               │
   │                            │                               │  Agent processes...
   │                            │                               │
   │                            │  GatewayReply {               │
   │                            │    reply_to: event_id         │
   │                            │  }                            │
   │                            │ ◀─────────────────────────────│
   │                            │                               │
   │                            │  4. Lookup cache(event_id)    │
   │                            │     ├─ HIT + fresh            │
   │     Reply API (FREE) ✅    │     │  → Reply API            │
   │ ◀──────────────────────────│     │                         │
   │                            │     ├─ HIT + expired          │
   │     Push API (quota) 💰    │     │  → Push API fallback    │
   │ ◀──────────────────────────│     │                         │
   │                            │     └─ MISS                   │
   │     Push API (quota) 💰    │        → Push API fallback    │
   │ ◀──────────────────────────│                               │
```

### Reply Strategy: Hybrid Reply/Push Messages

LINE offers two reply mechanisms:
- **Reply message**: uses a reply token, but the token expires in 1 minute (free).
- **Push message**: no time limit, can send to any user/group at any time (consumes quota).

Historically, OpenAB relied solely on **push messages** because agent processing can exceed the 1-minute reply token window. To optimize costs for free-tier accounts, OpenAB now uses a **Hybrid Strategy** implemented at the gateway level:
1. The gateway caches incoming `replyToken`s keyed by `event_id` with a 50-second TTL.
2. When OAB replies with a non-empty `reply_to` that matches a cached entry, the gateway routes the message via the free **Reply API**.
3. If the token is expired, missing, or `reply_to` is empty, the gateway falls back to the **Push API**.
4. A background task sweeps expired cache entries to prevent memory growth.
---

## 3. Architectural Differences from Discord/Slack

### Connectivity Model

Discord and Slack use **outbound WebSocket** — the bot connects out to the platform gateway. No inbound port, no public endpoint, no TLS termination needed.

LINE uses **inbound webhooks** — the LINE platform sends HTTP POST requests to the bot. This flips the connectivity model:

```
Discord/Slack (outbound):
  Pod ──WebSocket──▶ Platform Gateway
  • No K8s Service needed
  • No Ingress needed
  • No TLS termination needed

LINE (inbound):
  Platform ──HTTPS POST──▶ TLS ──▶ LB ──▶ K8s Service ──▶ Pod :8080
  • K8s Service required
  • Ingress required
  • TLS termination required (LINE mandates HTTPS with public CA cert)
  • Pod has a publicly reachable attack surface
```

### What's Needed to Bridge the Gap

| Component | Discord/Slack | LINE | Why |
|---|---|---|---|
| K8s Service | Not needed | Required | Route inbound traffic to pod |
| Ingress / Load Balancer | Not needed | Required | Expose webhook endpoint externally |
| TLS termination | Not needed | Required | LINE requires HTTPS with public CA cert |
| Webhook signature validation | N/A | Required | Verify requests are authentically from LINE (HMAC-SHA256) |
| HTTP server in the pod | Not needed | Required | Accept and parse incoming HTTP POST requests |

### Webhook Server: LINE-Specific vs General-Purpose Gateway

Since LINE forces OpenAB to listen on an inbound port, a natural question arises: should this be a LINE-only handler, or a general-purpose webhook gateway that future platforms (Telegram, WhatsApp, etc.) can also use?

| Option | Description | Pros | Cons |
|---|---|---|---|
| **A. LINE-specific** | Dedicated handler, only speaks LINE webhook format | Simple, ships fast | Not reusable for other platforms |
| **B. General gateway** | Shared HTTP server with `/webhook/{platform}` routing | One listener, one TLS endpoint, extensible | More upfront design |
| **C. External queue** | Webhook receiver → message queue → workers | Horizontally scalable, decoupled | Significant infra overhead |

**Recommendation:** Option A for v1 to unblock LINE support. Use a proper HTTP framework (e.g., `axum`) so that migrating to Option B is straightforward when a second webhook-based platform is added.

### v2 Target Architecture: Independent Webhook Bridge Service

The preferred long-term direction is to extract the webhook handler into an **independent service** (separate container/pod), keeping OAB core outbound-only:

```
v2 architecture:

  LINE Platform ──HTTPS POST──▶ [Webhook Bridge]  ──WebSocket──▶ OAB Pod
  Discord Gateway ◀──WebSocket── OAB Pod
  Telegram (future) ──HTTPS POST──▶ [Webhook Bridge] ──WebSocket──▶ OAB Pod

  OAB only sees WebSocket connections — does not know or care about inbound HTTP.
  Bridge acts as a "platform gateway" for webhook-based platforms, same role as
  Discord Gateway or Slack Socket Mode server.
```

Benefits:
- OAB core stays pure outbound — no port to open, no TLS, no K8s Service
- Webhook platforms are fully opt-in — Discord/Slack-only users deploy nothing extra
- Bridge is independently scalable (stateless inbound path)
- Natural general-purpose gateway for LINE, Telegram, WhatsApp, etc.

Open design questions (require a follow-up ADR):
- **IPC protocol**: WebSocket between bridge and OAB is the likely choice, but the event format and contract need to be defined
- **Reply path**: does OAB call LINE Push API directly (OAB remains LINE-aware), or does OAB reply through the bridge (cleaner separation, but bridge becomes stateful with credentials)?
- **Session ownership**: does the bridge or OAB own session routing?
- **Trust boundary, auth, reconnect, backpressure, dedup, ordering**

This is scoped as a **v2 initiative**. v1 ships LINE support inside OAB (Option A) to unblock LINE users. The v2 bridge architecture will be designed with the benefit of real usage data from v1.

---

## 4. ACP Session Model: Impact & Mitigations

### How Sessions Map Across Platforms

| Platform | Session Key | Trigger | Isolation |
|---|---|---|---|
| Discord | `discord:{thread_id}` | @mention → new thread | ✅ Per-thread, fully isolated |
| Slack | `slack:{thread_ts}` | @mention → new thread | ✅ Per-thread, fully isolated |
| LINE 1:1 | `line:{userId}` | Any message | ⚠️ Per-user (similar to Discord DM) |
| LINE Group | `line:{groupId}` | Any message | ❌ Shared across all group members |

The fundamental difference: Discord/Slack have **threads** that provide natural per-conversation isolation. LINE has **no thread primitive** — all messages in a chat are a flat stream.

### Why `line:{groupId}`, Not `line:{groupId}:{userId}`

The session key for LINE groups is `line:{groupId}` (shared) rather than `line:{groupId}:{userId}` (per-user). This is a deliberate choice:

- Bot replies are sent to the **entire group** via push message. The session boundary must match the **visibility boundary** — everyone in the group sees the same replies, so they should share the same context.
- `line:{groupId}:{userId}` would create per-user isolation, but the bot's replies would still be visible to everyone. This creates a mismatch: private context driving public replies that make no sense to other group members.
- If per-conversation isolation is required, the correct answer is to use Discord or Slack, not to simulate threads within LINE.

### Impact 1: Group Chat Context Pollution

In a LINE group, all members' messages feed into one shared agent session:

```
Alice: Review this Rust PR, focus on error handling
Bot:   [starts analyzing Rust code]
Bob:   Write me a Terraform module for EKS
Bot:   [context now has both Rust and Terraform — confused]
Carol: What's for lunch?
Bot:   [context now includes lunch discussion — wasting tokens]
```

Effects:
- Mixed intents from multiple users degrade agent response quality
- Context window fills with irrelevant messages, wasting tokens
- Bot replies are visible to everyone but may only make sense to one person
- No way to tell who the bot is responding to (no thread, no quote)

### Impact 2: 1:1 DM Memory Pressure

Each session = one `kiro-cli` process (~350MB). Unlike Discord/Slack where sessions are **on-demand** (@mention triggers), LINE 1:1 sessions are **always-on** — every DM user has a persistent session.

> **Note:** The ~350MB per-process figure is an observed estimate from typical kiro-cli usage. Actual memory varies by workload (context size, tool usage, file operations). Operators should profile their specific agent configuration before capacity planning.

| Active Users | Sessions | Memory | Pool (max_sessions=10) |
|---|---|---|---|
| 5 | 5 | ~1.75 GB | ✅ Within limit |
| 10 | 10 | ~3.5 GB | ⚠️ At limit |
| 15 | 15 | ~5.25 GB | ❌ Eviction starts |
| 30 | 30 | ~10.5 GB | ❌ Heavy thrashing |

For comparison: a Discord server with 100 members might have 2-3 concurrent @mentions → 2-3 sessions. A LINE bot with 30 friends → potentially 30 concurrent sessions.

When `max_sessions` is exceeded, the pool evicts the oldest idle session to make room.

#### What Happens When the Pool Is Full

With `max_sessions=10` and 10 active 1:1 DM users, the pool is at capacity (~3.5 GB memory):

```
Pool: [User1] [User2] [User3] ... [User10]   ← all 10 slots occupied

User11 sends a message:
  1. Pool finds the oldest idle session (e.g., User3, idle for 20 min)
  2. User3's session is suspended (session ID saved to disk)
  3. A new kiro-cli process is spawned for User11
  4. User11 gets a response — but with cold-start latency (~5-10s)

User3 comes back and sends a message:
  1. Pool finds the oldest idle session again (e.g., User7)
  2. User7 is suspended, User3's session is resumed from saved state
  3. User3's conversation context is restored — but again with cold-start latency

User12 sends a message while User11 is still active:
  1. Pool must evict someone — but fewer sessions are idle now
  2. If all 10 sessions are actively processing, the new message queues
     until a session becomes idle and can be evicted
```

Effects at scale:
- **10 concurrent users**: pool is full, no eviction yet. ~3.5 GB memory.
- **11-15 concurrent users**: occasional eviction. Users experience intermittent cold-start delays (~5-10s). Context is preserved via session resume, but the swap adds latency.
- **20+ concurrent users**: heavy thrashing. Most messages trigger an evict/resume cycle. The bot feels sluggish for everyone. Memory stays capped at ~3.5 GB but CPU spikes from constant process creation/teardown.
- **Worst case**: all sessions are actively processing (no idle sessions to evict). New messages must wait until a session finishes its current task before it can be swapped out.

### Impact 3: Always-On vs On-Demand

| | Discord/Slack | LINE |
|---|---|---|
| Trigger | @mention required | Every message triggers processing |
| Session creation | Only when explicitly invoked | Any DM or group message |
| Concurrent sessions | Few (most users aren't @mentioning) | Many (every bot friend has a session) |
| Scaling characteristic | Bounded by active @mentions | Bounded by total bot users |

This is the root cause of the scaling difference.

### Mitigation Options

| # | Option | Effect | Trade-off |
|---|---|---|---|
| 1 | **@mention gating** | Only process messages that @mention the bot in groups; 1:1 DMs remain always-on | Dramatically reduces group noise and session pressure; LINE API supports mention detection |
| 2 | **Lower session TTL** | `session_ttl_hours = 1` (default 24) | Faster idle session reclaim, but returning users lose conversation context |
| 3 | **Larger node** | More memory (e.g., 32GB) with higher `max_sessions` | Simple to implement; doesn't solve the fundamental scaling curve |
| 4 | **Queue-based decoupling** | Webhook → message queue → autoscaled worker pods | Production-grade horizontal scaling; significant infrastructure investment |
| 5 | **Lightweight agent mode** | Reduce per-session memory footprint | Fundamental fix, but out of scope for the LINE adapter |
| 6 | **Session admission control** | Reject or queue new sessions when pool is full and all sessions are active | Protects active users from being evicted mid-conversation; see details below |

#### Session Admission Control (Option 6)

The current pool behavior is "auto-evict oldest" — the 11th user always gets in by kicking someone out. This creates unpredictable disruptions for active users.

**Design goal:** protect active sessions from disruptive eviction while giving overload behavior that is explicit and predictable.

A more robust approach is a **hybrid admission strategy**:

```
New session request arrives:
  1. Pool has free slot                          → open session immediately
  2. Pool full, idle session exists (not processing
     and idle_for >= idle_threshold)             → evict oldest idle, open new
  3. Pool full, ALL sessions active or below
     idle_threshold                              → apply admission_policy
       - evict_idle_then_reject: reply "All agents are busy, please try again shortly"
       - evict_idle_then_queue:  add to waiting queue, notify when slot opens
```

Parameters:

| Parameter | Purpose | Recommended v1 Default | Notes |
|---|---|---|---|
| `idle_threshold` | Minimum idle time before a session is eligible for eviction. "Idle" means not currently processing an in-flight prompt — not just "no recent message." | `10m` | LINE is mobile-first; users commonly pause 2-3 minutes between messages. `5m` is too aggressive for general use. |
| `admission_policy` | What to do when pool is full and no session meets `idle_threshold`. Three-state enum: `evict_idle_then_reject`, `evict_idle_then_queue`, `always_evict_idle` (current behavior). | `evict_idle_then_reject` | Start with deterministic reject. Queue adds scheduling/fairness/timeout/stale-response complexity — defer until usage data justifies it. |
| `max_queue_size` | Maximum waiting queue depth. Only applies when `admission_policy = evict_idle_then_queue`. | `0` (disabled) | Queue is for short waits, not job backlogs. Keep small (`≤ 3`) if enabled. LINE's flat chat model means queued replies arrive late into a conversation that has moved on — poor UX at depth > 3. Queuing also requires storing the original message and userId, then sending a push message to re-trigger processing when a slot opens — this is not a lightweight feature. |
| `max_wait_duration` | Maximum time a queued request waits before being rejected with a busy message. Only applies when queuing is enabled. | `0s` (disabled) | Without this, queue has depth but no SLA. `30s`-`60s` is reasonable if queuing is enabled. Directly constrains user-perceived latency. |

**Recommended v1 defaults:**

```
idle_threshold       = 10m
admission_policy     = evict_idle_then_reject
max_queue_size       = 0
max_wait_duration    = 0s
```

v1 uses deterministic reject — no queuing. This keeps the behavior simple and predictable. Queuing can be enabled in a future iteration once real usage data is available to tune `max_queue_size` and `max_wait_duration`.

Combined with existing mechanisms:

| Mechanism | Trigger | Effect |
|---|---|---|
| **TTL expiry** | Session idle > `session_ttl_hours` | Auto-reclaim, frees slot |
| **Idle threshold eviction** | Pool full + idle session exists (idle ≥ `idle_threshold`, no in-flight prompt) | Reclaim oldest idle session for new user |
| **Hard cap reject** | Pool full + all sessions active or below threshold | New user gets "busy" message |
| **Queue** (future) | Pool full + queuing enabled | New user waits up to `max_wait_duration` |
| **Manual delete** | Operator runs session delete command | Force-free a specific slot |

### Recommended Approach

For v1:
- **1:1 DM**: per-user session is the correct model, analogous to Discord DM
- **Group chat**: per-group shared session is acceptable for v1. LINE group chat functions as a **"shared-room assistant"** — not a thread-equivalent collaboration tool. If the use case requires per-conversation isolation, the platform choice should be Discord or Slack, not LINE with simulated threads.
- **@mention gating**: strongly recommended as a fast follow-up — converts LINE from always-on to on-demand, aligning its scaling characteristics with Discord/Slack
- **Capacity planning**: document the memory math so operators can size their infrastructure appropriately. The default `max_sessions=10` is configurable via `pool.max_sessions` in `config.toml`.

---

## Summary

| Aspect | Discord/Slack | LINE |
|---|---|---|
| Connectivity | Outbound WebSocket | Inbound webhook (HTTP POST) |
| K8s Service / Ingress | Not needed | Required |
| TLS termination | Not needed | Required (public CA cert) |
| Thread support | Yes → per-conversation isolation | No → flat conversation stream |
| Session isolation | Per-thread | Per-user (1:1) / Per-group (shared) |
| Trigger mechanism | @mention (on-demand) | All messages (always-on) |
| Session scaling | ~2-3 concurrent | ~N total bot users |
| Memory pressure | Low | High (350MB × active users) |

---

## Consequences

### Positive

- LINE users can interact with OpenAB agents without switching to Discord or Slack
- The inbound webhook pattern opens the door for future webhook-based platforms (Telegram, WhatsApp, etc.)
- Using `axum` for the HTTP server provides a solid foundation for a general-purpose webhook gateway
- Hybrid reply/push strategy optimizes cost: the gateway opportunistically uses the free Reply API when the agent responds within the token TTL, falling back to Push API for longer-running tasks

### Negative

- Deployment complexity increases: LINE requires K8s Service, Ingress, TLS termination, and a publicly reachable endpoint — none of which Discord/Slack need
- Group chats share a single session, leading to context pollution when multiple users interact simultaneously
- LINE's always-on trigger model creates higher memory pressure and session pool contention compared to Discord/Slack's on-demand @mention model
- Operators must perform capacity planning (memory per session × expected user count) that wasn't necessary for Discord/Slack-only deployments
- v1 couples inbound HTTP handling to the OAB process, breaking the outbound-only connectivity model of Discord/Slack. Planned extraction to an independent webhook bridge service in v2.

---

## Compliance

To ensure this ADR is followed in implementation and future changes:

1. **Webhook correctness**: Webhook handling must validate the LINE signature against the exact raw request body bytes after the full HTTP body has been read according to protocol framing. Implementations must not use hand-rolled TCP parsing, lossy UTF-8 conversion, or reconstructed JSON for signature verification. A proper HTTP framework (e.g., `axum`) is the default acceptable implementation approach. Two specific failure modes make raw TCP handling unacceptable:
   - **Partial read**: a single `read()` call does not guarantee the full HTTP request arrives in one TCP segment. Truncated bodies cause HMAC validation to fail silently — messages are dropped with no error logged.
   - **Lossy UTF-8 HMAC mismatch**: if the raw buffer is converted to string via lossy UTF-8 before computing HMAC, any non-UTF-8 byte is replaced with `U+FFFD`, causing signature verification to fail on otherwise-valid requests.

   Both are **silent failures** — no crash, no log, just dropped messages. The webhook signature is defined over the original request body bytes, so any lossy decoding or body reconstruction changes the verification surface and is architecturally invalid. PRs introducing raw TCP HTTP handling must be rejected with a reference to this ADR.
2. **Session key convention**: LINE sessions must use `line:{userId}` for 1:1 DMs and `line:{groupId}` for group chats. Deviations require a new ADR.
3. **Documentation**: any LINE adapter PR must include or update operator-facing documentation covering:
   - Group chat shared session behavior and its limitations
   - Capacity planning guidance (memory math per session count)
4. **Group chat production use**: @mention gating should be implemented before promoting group chat support to production-ready status.
5. **Future webhook platforms**: when adding a second webhook-based platform, evaluate migrating to a general-purpose webhook gateway (Section 3, Option B) before building another platform-specific handler.
6. **Platform semantics**: LINE group support must not be described or documented as thread-equivalent to Discord/Slack. LINE groups are "shared-room assistants" with fundamentally different isolation and scaling characteristics.

---

## Notes

- **Version:** 0.2
- **Changelog:**
  - 0.2 (2026-04-28): Hybrid Reply/Push strategy implemented (#608). Updated status to Accepted. Added dispatch flow diagram. Reply strategy section rewritten from Push-only to hybrid. Core propagates `event_id` via `ChannelRef.origin_event_id` (#619).
  - 0.1 (2026-04-22): Initial proposed version

---

## References

This ADR follows the structure and process described in the following sources, adapted with project-specific sections (User Story, High-Level Design, Platform Comparison, Session Model Analysis) to fit OpenAB's needs.

- [Documenting Architecture Decisions](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions.html) — Michael Nygard (2011). The original blog post that popularized ADRs. Defines the minimal template: Context, Decision, Status, Consequences.
- [ADR GitHub Organization](https://adr.github.io/) — Community hub for ADR templates, tooling, and academic references. Includes the Y-statement format from Zdun et al.'s "Sustainable Architectural Decisions."
- [arc42 Section 9: Architecture Decisions](https://docs.arc42.org/section-9/) — European software architecture documentation standard. Emphasizes recording rejected alternatives and providing timestamps.
- [AWS Prescriptive Guidance — Using ADRs to streamline technical decision-making](https://docs.aws.amazon.com/prescriptive-guidance/latest/architectural-decision-records/adr-process.html) — Extends Nygard's template with Compliance and Notes (version, changelog) sections, and defines the ADR lifecycle (Proposed → Accepted → Superseded).
- [Azure Well-Architected Framework — Architecture Decision Record](https://learn.microsoft.com/en-us/azure/well-architected/architect-role/architecture-decision-record) — Microsoft's adoption of ADRs within the Azure Well-Architected Framework.

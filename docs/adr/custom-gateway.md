# ADR: Custom Gateway for Webhook-Based Platform Integration

- **Status:** Proposed
- **Date:** 2026-04-22
- **Author:** @chaodu-agent
- **Supersedes:** Sections of [ADR: LINE Adapter](./line-adapter.md) (v2 Target Architecture)

---

## 1. User Story & Requirements

As an OpenAB operator, I want to connect webhook-based platforms (LINE, Telegram, WhatsApp, GitHub, CI/CD, monitoring, custom apps) to OAB without modifying OAB core, so that OAB remains a pure outbound-only agent runtime and new event sources are opt-in.

As a platform integrator, I want to write a plugin/adapter for my platform and register it with the gateway, so that any webhook source can drive an OAB agent session without upstream code changes.

Requirements:
- OAB core must remain outbound-only — no inbound ports, no TLS, no K8s Service
- The gateway is a separate, independently deployable service
- Adding a new webhook platform requires only a gateway plugin, zero OAB changes
- The gateway handles all platform-specific concerns: signature validation, payload parsing, credential management, reply delivery
- OAB connects to the gateway via WebSocket, same pattern as Discord/Slack
- The gateway is opt-in — Discord/Slack-only deployments don't need it

---

## 2. High-Level Design

### Architecture Overview

```
                    External (inbound HTTPS)              Internal (cluster)
                    ────────────────────────              ──────────────────

LINE Platform ──POST──▶ ┌─────────────────────┐
Telegram      ──POST──▶ │                     │
GitHub Events ──POST──▶ │   Custom Gateway    │ ◀──WebSocket── OAB Pod
CI/CD webhook ──POST──▶ │     :443/8080       │
Custom app    ──POST──▶ │                     │
                         └─────────────────────┘
                                  │
Discord Gateway ◀──WebSocket───── OAB Pod  (unchanged, direct connection)
Slack Socket    ◀──WebSocket───── OAB Pod  (unchanged, direct connection)
```

### Inside the Gateway

```
┌─────────────────────────────────────────────────────┐
│  Custom Gateway                                     │
│                                                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐          │
│  │  LINE    │  │ Telegram │  │  GitHub  │  ...      │
│  │ Adapter  │  │ Adapter  │  │ Adapter  │          │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘          │
│       │              │              │               │
│       ▼              ▼              ▼               │
│  ┌─────────────────────────────────────────┐        │
│  │         Event Normalizer                │        │
│  │   platform payload → unified event      │        │
│  └──────────────────┬──────────────────────┘        │
│                     │                               │
│                     ▼                               │
│  ┌─────────────────────────────────────────┐        │
│  │       WebSocket Server (:9090)          │        │
│  │   OAB connects here as a client         │        │
│  └──────────────────┬──────────────────────┘        │
│                     │                               │
│                     ▼                               │
│  ┌─────────────────────────────────────────┐        │
│  │         Reply Router                    │        │
│  │   OAB reply → correct platform API      │        │
│  └─────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────┘
```

### Message Flow

```
Inbound (webhook → OAB):
  1. External platform POSTs to gateway endpoint (e.g., /webhook/line)
  2. Platform adapter validates signature, parses payload
  3. Event Normalizer converts to unified event schema
  4. Gateway pushes event to OAB over WebSocket
  5. OAB routes to AdapterRouter → Session Pool → kiro-cli

Outbound (OAB → platform):
  1. Agent produces a response
  2. OAB sends unified reply over WebSocket to gateway
  3. Reply Router looks up platform + credentials for the target channel
  4. Gateway calls the platform-specific API (LINE Push, Telegram sendMessage, etc.)
  5. Delivery confirmation sent back to OAB over WebSocket
```

---

## 3. Internal Event Schema

The contract between the gateway and OAB. All platform-specific details are normalized away before crossing this boundary.

### Inbound Event (Gateway → OAB)

```json
{
  "schema": "openab.gateway.event.v1",
  "event_id": "evt_abc123",
  "timestamp": "2026-04-22T08:00:00Z",
  "platform": "line",
  "event_type": "message",
  "channel": {
    "id": "Cxyz789",
    "type": "group",
    "thread_id": null
  },
  "sender": {
    "id": "Uabc123",
    "name": "Alice",
    "display_name": "Alice Chen",
    "is_bot": false
  },
  "content": {
    "type": "text",
    "text": "explain VPC peering"
  },
  "mentions": [],
  "message_id": "msg_456",
  "raw": {}
}
```

Key fields in the base schema:
- **`channel.thread_id`**: thread identifier for platforms that support threads (Discord thread ID, Slack `thread_ts`). `null` for platforms without threads (LINE). OAB uses this for session key construction — without it, per-thread session isolation cannot work through the gateway.
- **`mentions`**: array of mentioned entity IDs (users, bots). Required for @mention gating — the gateway adapter parses platform-specific mention formats and normalizes them here. Without it, OAB cannot determine whether the bot was mentioned, breaking the primary mitigation for LINE group chat noise and Discord/Slack trigger logic.

### Outbound Reply (OAB → Gateway)

```json
{
  "schema": "openab.gateway.reply.v1",
  "reply_to": "evt_abc123",
  "platform": "line",
  "channel": {
    "id": "Cxyz789"
  },
  "content": {
    "type": "text",
    "text": "VPC peering is..."
  }
}
```

Key fields in the outbound reply:
- **`reply_to`**: the `event_id` of the inbound `GatewayEvent` that triggered this reply. The gateway can use this for reply correlation — e.g., looking up a cached LINE reply token to prefer the free Reply API over the quota-consuming Push API. Empty string if the reply is not associated with a specific inbound event (e.g., cron-triggered messages).

### Design Principles for the Schema

- **Platform field is metadata, not routing logic** — OAB uses it for session key construction and sender context, but does not branch behavior on it
- **Content is polymorphic** — `type` can be `text`, `image`, `file`, etc. Platforms that don't support a content type get a graceful fallback (e.g., image → URL link)
- **`raw` is optional** — the original platform payload, for adapters that need platform-specific fields. OAB core should never depend on `raw`
- **Schema versioned** — `openab.gateway.event.v1` allows backward-compatible evolution

### Schema Concerns Deferred to Protocol Spec

The following fields/concepts are known to be needed but are not fully defined in this ADR. They must be addressed in the protocol specification before v2 ships:

| Concern | Why It's Needed |
|---|---|
| `conversation_key` / `session_hint` | Explicit session boundary signal from gateway, so OAB and GW don't independently derive session keys that drift apart. May generalize `thread_id` + `channel.id` into a single key. |
| `trace_id` / `correlation_id` | End-to-end tracing across inbound event → agent run → outbound reply → delivery ack |
| `capabilities` / `delivery_constraints` | Platform feature flags (supports edit? reply? threads? attachments? max message length?) so OAB can do graceful fallback |
| `reply_context` | Reply token, quote target, original message reference — not all platforms only need `channel.id` to deliver a reply |
| `tenant` / `gateway_instance` | Multi-tenancy routing if a shared gateway serves multiple OAB instances |

---

## 4. Gateway Adapter Interface

Each platform adapter implements a common interface:

```
Adapter Interface:
  validate(request)     → bool          # signature/auth check
  parse(request)        → Event         # platform payload → unified event
  send(reply)           → DeliveryAck   # unified reply → platform API call
  health()              → Status        # platform connectivity check
```

### Adapter Registration

```yaml
# gateway-config.yaml
adapters:
  line:
    enabled: true
    path: /webhook/line
    credentials:
      channel_secret: ${LINE_CHANNEL_SECRET}
      channel_access_token: ${LINE_CHANNEL_ACCESS_TOKEN}
  telegram:
    enabled: true
    path: /webhook/telegram
    credentials:
      bot_token: ${TELEGRAM_BOT_TOKEN}
  github:
    enabled: true
    path: /webhook/github
    credentials:
      webhook_secret: ${GITHUB_WEBHOOK_SECRET}
```

Adding a new platform = implement the adapter interface + add a config block. Zero OAB changes.

---

## 5. What This Enables Beyond Chat

The gateway turns OAB from a "chat bot" into an **event-driven agent platform**:

| Event Source | Webhook Payload | OAB Can Do |
|---|---|---|
| LINE / Telegram / WhatsApp | User message | AI coding assistant (existing) |
| GitHub webhook | PR opened, issue created | Auto-triage, auto-review, auto-label |
| CI/CD (Actions, Jenkins) | Build failed | Analyze logs, suggest fix, notify |
| Monitoring (PagerDuty, CloudWatch Alarm) | Alert fired | Runbook execution, incident response |
| Custom application | Any JSON payload | User-defined agent workflows |

The key insight: OAB's session pool and agent runtime are platform-agnostic. The only platform-specific code lives in gateway adapters. Once the gateway exists, any HTTP event source can drive an agent session.

This also lowers the barrier to entry: triggering an agent no longer requires setting up a chat platform bot. A simple `curl -X POST https://gateway/webhook/custom -d '{"text": "run daily security scan"}'` from a cron job is enough to start an agent session — no SDK, no bot token, no platform account needed.

Scheduled triggers are supported natively via any external scheduler (K8s CronJob, EventBridge, GitHub Actions cron) — the gateway sees a regular HTTP POST with no knowledge of scheduling logic.

---

## 5a. Example Integrations

Two examples to illustrate how the gateway handles different event types — one chat platform, one non-chat event source.

### Telegram: Chat-Style Webhook

```
Telegram servers ──POST /webhook/telegram──▶ Gateway
  │
  ├─ Adapter: validate bot_token, parse Update JSON
  ├─ Normalize: extract chat_id, sender, text, mentions
  │
  ▼
Gateway ──WebSocket──▶ OAB
  event: { platform: "telegram", channel.id: "chat_12345",
           sender.name: "Bob", content.text: "deploy to staging" }
  │
  ▼
OAB reply ──WebSocket──▶ Gateway
  │
  ├─ Reply Router: POST https://api.telegram.org/bot.../sendMessage
  └─ { chat_id: "chat_12345", text: "Deploying now..." }
```

Telegram is structurally similar to LINE — inbound webhook, no threads, push-style reply. The gateway adapter handles Telegram-specific auth and payload format; OAB sees the same unified event schema.

### GitHub: Non-Chat Event Source

```
GitHub ──POST /webhook/github──▶ Gateway
  │  (X-Hub-Signature-256 header)
  │
  ├─ Adapter: validate HMAC signature, parse pull_request.opened event
  ├─ Normalize: repo as channel, PR author as sender, PR title+body as content
  │
  ▼
Gateway ──WebSocket──▶ OAB
  event: { platform: "github", channel.id: "openabdev/openab",
           event_type: "pull_request", sender.name: "juntinyeh",
           content.text: "PR #521: Feature/line — adds LINE adapter..." }
  │
  ▼
OAB reply ──WebSocket──▶ Gateway
  │
  ├─ Reply Router: POST https://api.github.com/repos/.../issues/521/comments
  └─ { body: "Auto-triage: this PR adds a new adapter..." }
```

GitHub shows the gateway handling a non-chat event. The adapter maps repo → channel, PR author → sender, and PR description → content. OAB processes it like any other message. The reply goes back as a PR comment via GitHub API.

---

## 6. Architectural Differences from v1

| Aspect | v1 (LINE in OAB) | Custom Gateway |
|---|---|---|
| OAB inbound ports | Yes (webhook :8080) | None — pure outbound |
| Platform-specific code in OAB | `line.rs`, `config.rs` changes | Zero — all in gateway |
| Adding a new platform | Modify OAB core, new adapter in Rust | Gateway plugin only, OAB untouched |
| TLS / K8s Service for OAB | Required for LINE | Not required — gateway handles it |
| Deployment for DC/Slack-only users | Carry LINE code even if unused | Gateway not deployed, zero overhead |
| Credential management | OAB holds LINE tokens | Gateway holds all webhook credentials |
| Scaling webhook handling | Coupled to OAB pod | Gateway scales independently |

---

## 7. Open Design Questions

| Question | Options | Impact |
|---|---|---|
| **WebSocket direction** | OAB connects to GW (OAB is client) vs GW connects to OAB (GW is client) | OAB-as-client is simpler — matches Discord/Slack pattern, OAB initiates all connections |
| **Multi-tenancy** | One GW per OAB instance vs shared GW serving multiple OAB instances | Shared GW needs routing table; per-instance is simpler for v2 |
| **Reconnect / backpressure** | What happens when OAB ↔ GW WebSocket drops? | Need reconnect with event replay or at-least-once delivery |
| **Event ordering** | Strict per-channel ordering vs best-effort | Strict ordering requires per-channel queuing in GW |
| **Plugin distribution** | Built-in adapters vs dynamic loading vs separate binaries | Built-in for v2, plugin mechanism for v3 |

---

## 8. Rollout Plan

| Phase | Scope | Deliverable |
|---|---|---|
| **v1 (now)** | LINE adapter inside OAB | PR #521 — unblocks LINE users |
| **v2** | Standalone gateway + OAB generic gateway adapter + LINE migrated out | Gateway service, LINE adapter, internal event schema, OAB connects via WebSocket |
| **v3** | Multi-platform gateway | Telegram, GitHub, custom adapters |
| **v4** | Plugin / distribution model | Third-party adapters without forking gateway |

### Migration Path from v1 to v2

LINE only allows a single webhook URL per channel, so live dual-run (v1 and v2 receiving the same production traffic simultaneously) is not possible. Validation must use a separate test channel or synthetic replay.

```
Phase 1 — Deploy gateway alongside v1 OAB:
  OAB Pod [LINE adapter built-in]  ◀──POST── LINE Platform   (v1, production traffic)
  OAB Pod [gateway adapter]  ──WS──▶  Gateway Pod [LINE adapter]  (v2, deployed but idle)

Phase 2 — Validate v2 path (staging / synthetic):
  Use a separate LINE test channel (or synthetic HTTP replay) pointed at the gateway
  Confirm: signature validation, session routing, reply delivery all work
  Production traffic still flows through v1
  Define go/no-go criteria before executing Phase 3
  (e.g., N messages processed, zero signature failures, reply latency within threshold)

Phase 3 — Cut over (atomic switch):
  Disable v1 LINE adapter in OAB config
  Point production LINE webhook URL to gateway endpoint
  OAB now receives LINE events only through gateway WebSocket

Phase 4 — Clean up:
  Remove line.rs from OAB codebase
  Remove LINE-specific config/secrets from OAB
  OAB is back to outbound-only
```

Key constraint: Phase 3 (cut-over) must be atomic per platform — LINE's single webhook URL in the Developers Console is the routing switch. There is no gradual traffic split; it's all-v1 or all-v2.

---

## Consequences

### Positive

- OAB core permanently stays outbound-only — simpler security model, no attack surface
- New webhook platforms are opt-in and require zero OAB changes
- Gateway can be independently scaled, versioned, and deployed
- Opens OAB to non-chat event sources (GitHub, CI/CD, monitoring)
- Gateway is potentially useful as a standalone open-source project beyond OAB

### Negative

- Additional service to deploy and operate (gateway pod)
- Internal WebSocket protocol adds a layer of complexity and a potential failure point
- Reply path through gateway adds one network hop of latency (negligible in-cluster)
- Gateway becomes a credential store for all webhook platforms — single point of compromise if breached
- Event schema design is a long-term commitment — breaking changes affect all adapters

### Credential Store Security Risks

The gateway holding all platform credentials is the correct architectural choice (it keeps OAB platform-agnostic), but concentrates risk:

- **Credential concentration**: a gateway breach exposes all platform tokens. Mitigate with per-adapter secret isolation (separate K8s Secrets, not one shared Secret).
- **Audit and rotation**: all credential access must be auditable. Tokens must support rotation without gateway downtime.
- **Least privilege**: each adapter should only have access to its own platform credentials, not other adapters'.
- **OAB ↔ GW transport security**: the internal WebSocket connection must use mTLS or equivalent authentication. Without it, any pod in the cluster can impersonate OAB and send replies through the gateway.

---

## Compliance

1. **OAB outbound-only**: after adoption of the custom gateway architecture, new platform integrations must not add inbound platform-traffic handling to OAB core unless explicitly approved by a superseding ADR.
2. **Event schema stability**: the `openab.gateway.event.v1` schema is currently a draft envelope for v2 development. The protocol spec must finalize all required fields (including deferred concerns in Section 3) before the schema is declared stable. Once declared stable, breaking changes require a version bump (`v2`) and a migration path.
3. **Credential isolation**: platform credentials (tokens, secrets) must reside in the gateway, not in OAB. OAB must not hold or access platform-specific authentication material.
4. **Adapter interface compliance**: all gateway adapters must implement `validate`, `parse`, `send`, and `health`. Adapters that skip signature validation must be explicitly flagged as insecure.
5. **Webhook correctness**: all adapters must validate signatures against exact raw request body bytes, per the constraints defined in [ADR: LINE Adapter](./line-adapter.md) Compliance #1.

---

## Notes

- **Version:** 0.1
- **Changelog:**
  - 0.1 (2026-04-22): Initial proposed version

---

## References

- [ADR: LINE Messaging API Adapter](./line-adapter.md) — v1 LINE adapter design and v2 bridge concept that led to this ADR
- [Documenting Architecture Decisions](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions.html) — Michael Nygard (2011)
- [ADR GitHub Organization](https://adr.github.io/) — Community hub for ADR templates and tooling
- [AWS Prescriptive Guidance — Using ADRs](https://docs.aws.amazon.com/prescriptive-guidance/latest/architectural-decision-records/adr-process.html) — ADR lifecycle and compliance sections

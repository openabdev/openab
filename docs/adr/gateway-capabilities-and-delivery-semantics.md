# ADR: Gateway Capabilities and Delivery Semantics

- **Status:** Proposed
- **Date:** 2026-08-06
- **Author:** @NeoHsu
- **Related:**
  - [Custom Gateway](custom-gateway.md)
  - [Unified Binary](unified-binary.md)
  - [Multi-Platform Adapters](multi-platform-adapters.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)
  - [Teams message reactions preview](teams-message-reactions-preview.md)

---

## Context

OpenAB has two paths for webhook-based chat platforms:

1. **Unified:** platform adapters and Core run in one process.
2. **Standalone:** Core connects to `openab-gateway` through `/ws`.

Before this decision, Core inferred behavior from adapter-wide methods and platform-name allowlists. That caused three classes of error:

- a shared Unified adapter could apply Telegram streaming settings to Teams;
- Standalone Core could not know whether a Gateway acknowledged send, edit, or delete operations;
- a transport timeout could not be distinguished from an explicit platform rejection.

The Standalone event channel is a bounded in-process broadcast channel. It has no durable inbox, replay, shared deduplication store, or consumer-group semantics. Multiple Core consumers therefore receive duplicate events rather than distributed work.

## Decision

### 1. Proposed baseline product decisions

The following decisions define this proposed single-process baseline:

| ID | Decision |
| --- | --- |
| D1 | The baseline supports one process replica and one active Standalone Core consumer per platform. Ingress after local enqueue is best-effort; there is no crash replay or exactly-once claim. A second consumer is warned at high severity and reported as unsupported, but is not rejected in this backward-compatible release. |
| D2 | Standalone uses an optional, client-initiated capability handshake. A peer that does not complete a supported handshake remains in legacy mode; missing ACKs are not delivery failures in legacy mode. |
| D3 | `GatewayEvent.event_id` is correlation metadata, never a platform activity ID. New↔new create/send returns the real platform message ID. Teams client presentation is outside transport acknowledgement semantics. |
| D4 | Teams supports Microsoft commercial public cloud only. Sovereign-cloud and custom-proxy endpoints require an explicit future cloud profile. |
| D5 | User-visible status and content streaming are independent capabilities. Teams processing status must not reuse a streaming placeholder implicitly. |

### 2. Platform-aware capability contract

Each adapter exposes capabilities for the actual `ChannelRef.platform`:

```rust
struct AdapterCapabilities {
    send_ack: bool,
    edit_ack: bool,
    delete_ack: bool,
    supports_target_message_id: bool,
    supports_reactions: bool,
    can_edit: bool,
    can_delete: bool,
    streaming_mode: StreamingMode,
    show_streaming_placeholder: bool,
    message_limit: MessageLimit,
    status_backend: StatusBackend,
}
```

Capability defaults fail closed:

- no required ACK;
- no additive command-target field or native reaction support;
- no edit or delete support;
- streaming disabled;
- status side effects disabled;
- a conservative 4,096-character message limit.

Direct adapters derive a backward-compatible capability view from their existing methods. Unified and Standalone shared adapters override it by platform.

A valid negotiated hello is authoritative. If a platform is omitted from a valid hello, Core uses fail-closed defaults rather than optimistic legacy behavior. Legacy behavior is used only before a supported hello is accepted.

### 3. Optional Standalone hello exchange

Core sends this additive control frame immediately after connecting:

```json
{
  "schema": "openab.gateway.client_hello.v1",
  "protocol_version": 1,
  "client_name": "openab-core/<version>",
  "requested_platforms": ["teams"]
}
```

A new Gateway responds:

```json
{
  "schema": "openab.gateway.hello.v1",
  "protocol_version": 1,
  "capabilities": {
    "teams": {
      "send_ack": true,
      "edit_ack": true,
      "delete_ack": true,
      "supports_target_message_id": true,
      "supports_reactions": false,
      "can_edit": true,
      "can_delete": true,
      "streaming_mode": "disabled",
      "show_streaming_placeholder": true,
      "message_limit": { "unit": "characters", "max": 4096 },
      "status_backend": "none"
    }
  },
  "topology": {
    "active_consumers": 1,
    "supported": true,
    "delivery_mode": "best_effort_broadcast"
  }
}
```

Rules:

- unknown JSON fields are additive and may be ignored;
- an empty `requested_platforms` list requests all configured adapters; the stock Core uses this because one Standalone socket can carry events from several platforms;
- protocol version mismatch, malformed hello, or no hello keeps Core in legacy mode;
- Gateway continues to accept `openab.gateway.reply.v1` as the first frame, so an old Core works with a new Gateway;
- a new Core may send `client_hello` to an old Gateway; the old Gateway may log it as an invalid reply but must keep the connection usable;
- operations emitted before a valid hello is processed use legacy semantics;
- control frames are prioritized over broadcast events once received.

Recommended Standalone rollout order remains Gateway first, then Core, but either side may be upgraded first.

### 4. Structured write outcome

Gateway keeps the existing `openab.gateway.response.v1` fields and adds optional fields:

- `outcome`: `delivered`, `rejected`, or `unknown`;
- `error_code`;
- `retry_after_ms`.

The internal result is:

```rust
enum WriteOutcome {
    Delivered { message_id: Option<String> },
    Rejected {
        code: String,
        message: String,
        retry_after_ms: Option<u64>,
    },
    Unknown { code: String, message: String },
}
```

Semantics:

- create/send delivery requires a non-empty real message ID when that operation advertises required ACK support;
- edit/delete delivery does not require a message ID in its ACK;
- explicit platform refusal is `Rejected`;
- an ambiguous POST timeout or disconnect is `Unknown` and must not be retried blindly;
- legacy responses without `outcome` map from the existing `success`, `message_id`, and `error` fields;
- Core waits only for an operation whose capability advertises the corresponding ACK;
- Teams advertises `send_ack = true` only after its event-route send path emits a terminal structured response with a non-empty Bot Framework activity ID on delivery;
- Teams advertises edit/delete ACK and `supports_target_message_id` only after bot-owned mutation enforcement emits a terminal response on every command path;
- Teams advertises `supports_reactions = true` and `status_backend = reactions` only under the explicit public-preview `reactions_enabled` opt-in; the default remains false/`none`;
- `supports_reactions` is independent from the selected progress backend so permanent batch receipts can coexist with a processing message; new Core normalizes an old peer's `status_backend = reactions` to reaction support;
- configured Teams processing messages are selected Core-side only after a valid hello advertises required send/edit/delete ACKs, additive command targets, and bot-owned edit/delete; no valid hello means no message status;
- negotiated required ACK timeout defaults to 12 seconds and is configurable as `[gateway].gateway_ack_timeout_secs`;
- configuration rejects zero, a budget at or above `pool.prompt_hard_timeout_secs`, and a Teams budget at or below the 10-second Connector timeout;
- legacy response waits preserve their previous best-effort behavior.

The 12-second Gateway budget must remain greater than the Teams Bot Connector request timeout (10 seconds) and less than the ACP turn hard timeout.

### 5. Topology guardrail

Each Gateway process counts active `/ws` Core consumers:

- one consumer: `topology.supported = true`;
- more than one: emit an error-level log and return `topology.supported = false` with `delivery_mode = "best_effort_broadcast"`;
- disconnect decrements the count through a drop guard, including task cancellation paths.

This detects unsupported fan-out inside one Gateway process. It cannot detect multiple independent Gateway replicas because the baseline deliberately has no shared state. The Helm deployments therefore remain fixed at `replicas: 1` with `Recreate` strategy. External deployments must follow the same constraint.

Rejecting the second consumer would be a breaking change and is deferred.

## Compatibility Matrix

| Core | Gateway | Behavior |
| --- | --- | --- |
| old | old | Existing protocol and fire-and-forget behavior. |
| old | new | Gateway accepts a reply without hello; additive response fields are ignored. |
| new | old | Core sends optional hello, receives none, and stays in legacy mode; missing ACK is not failure. |
| new | new | Valid hello enables platform-aware capabilities, operation-specific required ACKs, structured outcomes, and topology reporting. |

## Security and Reliability Boundaries

- Capability negotiation is not authentication or authorization. Existing WebSocket token, platform webhook authentication, tenant checks, L2 scope, and L3 identity gates remain authoritative.
- Hello frames contain no platform credentials, service URLs, route records, or user identifiers.
- Advertising a capability does not make an operation safe by itself; the adapter must emit the corresponding ACK on every terminal path before the flag is enabled.
- `Unknown` preserves ambiguity instead of creating duplicates through automatic retry.
- This baseline does not claim durable enqueue, replay, duplicate-safe multi-consumer operation, or exactly-once delivery.

## Consequences

### Positive

- Teams no longer inherits generic Gateway or Telegram streaming/status behavior; reaction availability, processing-message selection, and progressive content each require their own explicit opt-in and capability gate.
- Core no longer needs write-path platform allowlists such as `EDIT_RESPONSE_PLATFORMS`.
- New platform features can be introduced additively without forcing a lockstep Core/Gateway deployment.
- Operators and Core can identify unsupported multi-consumer topology.
- Delivery uncertainty is represented explicitly and can be handled without unsafe retry.

### Negative

- Capability DTOs are mirrored in Core and Gateway and require wire-compatibility tests.
- The first operation may use legacy behavior if it races ahead of hello processing.
- Existing adapters that cannot return a stable message ID must advertise conservative send-once behavior until their delivery path is upgraded.
- Cross-replica topology remains undetectable without a shared coordination system.

## Alternatives Rejected

1. **Platform-name allowlists in Core.** Rejected because they drift whenever an adapter changes behavior and cannot represent deployment-specific support.
2. **Mandatory hello before accepting replies.** Rejected because it breaks old Core deployments during rolling upgrade.
3. **Treat every timeout as rejection.** Rejected because the platform may have committed an ambiguous POST.
4. **Retry ambiguous POST automatically.** Rejected because it can duplicate user-visible activities.
5. **Reject the second consumer immediately.** Deferred because this is a breaking operational change.
6. **Claim HA from multiple broadcast consumers.** Rejected because broadcast fan-out is not work distribution and has no shared idempotency state.

## Verification

Automated verification must cover:

- capability DTO defaults and wire round trips;
- all three structured outcomes plus legacy response decoding;
- old Core→new Gateway reply without hello;
- new Core→old Gateway legacy fallback;
- new↔new capability selection;
- requested-platform filtering;
- second-consumer unsupported topology and disconnect decrement;
- Unified Teams isolation from Telegram streaming settings;
- additive reaction-support decoding and processing-message fail-closed capability selection;
- Teams progressive-response selection only under explicit opt-in plus every required write primitive, in Standalone and Unified modes;
- configurable 12-second ACK default.

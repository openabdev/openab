# ADR: Teams Progressive Edit Response

- **Status:** Proposed
- **Date:** 2026-08-08
- **Author:** @NeoHsu
- **Related:**
  - [Gateway capabilities and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)
  - [Teams processing-message indicator](teams-processing-indicator.md)
  - [Turn-boundary message batching](turn-boundary-batching.md)

---

## Context

OpenAB already has a platform-neutral post-and-edit streaming path. Teams could
not safely use it before
[real-send acknowledgement](teams-real-send-acknowledgement.md) and
[bot-owned mutations](teams-owned-message-mutations.md), because a placeholder
POST did not return a real Bot Connector activity ID and later PUT/DELETE operations had no
operation-specific acknowledgement or bot-ownership boundary.

The linked real-send and mutation decisions supply those primitives. This
proposal may opt Teams into progressive content, but it must not reintroduce synthetic IDs, fixed best-effort waits, blind retries, or a
fresh final message after an ambiguous write. It must also preserve the
capability separation rule: the [processing message](teams-processing-indicator.md) and content placeholder are
independent activities and lifecycles.

## Decision

### Explicit, default-off setting

Add one first-class Teams setting:

```toml
[teams]
streaming = false
```

The environment fallback is `TEAMS_STREAMING`. Missing or malformed environment
values resolve to `false`; malformed TOML values fail config parsing. The
existing generic `[gateway].streaming` setting must not implicitly enable Teams.
Existing deployments therefore remain send-once.

Streaming does not enable reactions, processing messages, attachments, Graph,
RSC, delegated tokens, or new manifest permissions.

### Capability selection and rolling upgrades

Teams progressive response requires all of these primitives:

- a valid Gateway hello;
- required send, edit, and delete acknowledgements;
- `supports_target_message_id`;
- bot-owned edit and delete support; and
- `show_streaming_placeholder = true`.

When configured and all primitives are present, new Core selects internal
`StreamingMode::Edit`. Otherwise it fails closed to `StreamingMode::Disabled`.
Unified selects the same mode only when its in-process Teams adapter provides
the same primitives.

The Gateway hello continues to advertise Teams `streaming_mode = disabled` so
an old Core with an unrelated generic gateway streaming switch cannot begin
streaming merely because Gateway was upgraded first. New Core derives the
opt-in mode from the explicit Teams setting plus the existing primitive flags;
no protocol version or new command is required.

Compatibility behavior is:

| Core | Gateway | Configured Teams result |
| --- | --- | --- |
| old | new | Send-once; new Gateway does not advertise selected Teams streaming. |
| new | old without valid hello | Send-once, fail closed. |
| new | valid hello with every required primitive | Edit streaming may be selected. |
| new | hello missing any primitive | Send-once, fail closed. |
| Unified | embedded Teams adapter | Edit streaming only under the explicit Teams opt-in. |

### One real placeholder per turn

After L1/L2/L3 admission and successful ACP prompt start, Core creates at most
one content placeholder for the turn through the authenticated event route.
Required send ACK must return a non-empty real activity ID. That ID is the only
placeholder target the turn may edit or delete.

A batched turn still runs one ACP turn and therefore creates one placeholder,
anchored to the final event's authenticated `origin_event_id`. Concurrent or
successive turns never share placeholder state. Multi-bot participation keeps
the existing per-turn streaming disable and falls back to send-once.

Placeholder POST outcomes are handled as follows:

- `Delivered` with a real ID: begin progressive edits;
- `Rejected`: no activity was created, so complete the ACP turn and safely use
  the normal send-once final path;
- `Unknown`, required-ACK timeout, or closed ACK channel: do not create another
  placeholder or fresh-send the final answer, because the first POST may have
  committed without returning its ID.

### Coalesced cosmetic edits

The existing edit loop remains the common implementation:

- publish only changed display content;
- coalesce at a minimum 1.5-second interval;
- wait for the negotiated edit ACK budget, never the legacy Feishu 800 ms
  observation window;
- let Gateway perform only its existing explicit short `429 Retry-After`
  bounded retry;
- never retry the same content after a rejected or unknown PUT; a later edit is
  allowed only when newer content supersedes it; and
- stop cosmetic edits after three consecutive failed changed-content writes.

All Teams POST, PUT, DELETE, and reaction writes continue to share the existing
per-conversation write shard. Core aborts and joins the cosmetic edit task before
the authoritative final write so a stale edit cannot overtake finalization.

### Outcome-aware finalization

The first final chunk is authoritative. Core must retain the structured outcome
instead of flattening it to an undifferentiated error.

| Final operation | Required behavior |
| --- | --- |
| Placeholder PUT `Delivered` | Send overflow chunks sequentially. |
| Placeholder PUT `Rejected` | Attempt one placeholder DELETE, then use one fresh POST recovery unless DELETE is `Unknown`. |
| Placeholder PUT `Unknown` | Do not DELETE, retry PUT, or fresh-send. Mark delivery ambiguous. |
| Recovery DELETE `Delivered` | Fresh-send the first final chunk once. |
| Recovery DELETE `Rejected` | Fresh-send once; the rejected delete may leave partial placeholder overlap, but no full final activity exists. |
| Recovery DELETE `Unknown` | Do not fresh-send because deletion may have committed. |
| Recovery POST `Rejected` or `Unknown` | Do not retry. |

An explicit `[[reply_to:...]]` directive remains a deliberate new-message path:
Core sends the quoted final activity first and deletes the placeholder only
after that send is `Delivered`. An unknown quoted POST is not retried and does
not trigger placeholder deletion.

Overflow chunks are sent in order only after the first final chunk is known to
be delivered. Core stops at the first rejected or unknown overflow POST; every
chunk contributes to delivery health. It never skips a failed chunk and sends a
later one.

An ambiguous progressive write returns a delivery error that suppresses the
router's usual fresh warning message. Reactions or a separate processing status
may still show failure, but Core must not turn ambiguity into another Teams
activity.

### Processing-status coexistence

`processing_indicator = "message"` and `streaming = true` may coexist. They
produce two distinct activities:

1. the processing status follows its thinking/tool/terminal lifecycle; and
2. the streaming placeholder contains progressively rendered answer content.

Core marks the processing activity terminal before the authoritative final
content write and clears it only after every final chunk is delivered. A final
delivery failure leaves or updates the separate status to a recognizable
failure state when possible. Neither controller edits or deletes the other's
activity ID.

Reaction preview remains independent. When enabled, queued `👀` receipts stay
permanent while content progress uses the placeholder.

## Security and reliability boundaries

- Trust gates run before placeholder creation.
- Placeholder POST uses only the authenticated process-local event route.
- PUT/DELETE reuse bot-owned activity validation, tenant/conversation matching,
  TTL bounds, and write serialization.
- No placeholder ID survives restart, route expiry, or replica changes.
- `Unknown` is never reclassified as rejection and never causes blind retry or
  fresh-send.
- This proposal does not claim exactly-once delivery, crash cleanup, replay,
  durable ownership, or multi-consumer work distribution.
- Microsoft commercial public cloud remains the only supported cloud profile.

## Acceptance criteria

Automated verification must cover:

- config, environment fallback, malformed-value fail-closed behavior, and the
  default-off invariant;
- Teams not inheriting generic Gateway or Telegram streaming settings;
- valid-hello and every-required-primitive capability gating in Standalone;
- identical Unified capability selection;
- one placeholder POST returning and reusing one real activity ID;
- coalescing, changed-content-only writes, three-failure cutoff, and edit-loop
  abort/join before finalization;
- required ACK timeout rather than the legacy 800 ms path;
- final PUT `Delivered`, `Rejected`, and `Unknown` branches;
- recovery DELETE `Delivered`, `Rejected`, and `Unknown` branches;
- no fresh-send or warning activity after ambiguous POST, PUT, or DELETE;
- explicit-reply finalization;
- ordered overflow delivery stopping on first failure;
- processing-message and permanent receipt coexistence; and
- no placeholder side effect before trust admission.

## Consequences

### Positive

- Teams gains progressive answer content using already-authenticated Connector
  writes and real activity IDs.
- Explicit outcome handling preserves duplicate safety during transport
  ambiguity.
- Rolling upgrades cannot accidentally enable streaming in an old Core.
- Status, receipts, and content progress remain independently configurable.

### Negative

- Enabled turns add repeated acknowledged PUTs and can increase Connector write
  pressure.
- An ambiguous write may leave a partial placeholder and intentionally suppress
  a fresh final answer.
- Recovery after an explicitly rejected DELETE may show partial placeholder
  overlap beside the complete fresh answer.
- Process restart can orphan a placeholder because state remains intentionally
  non-durable.

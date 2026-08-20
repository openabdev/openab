# ADR: Teams Bot-Owned Message Mutations

- **Status:** Proposed
- **Date:** 2026-08-09
- **Author:** @NeoHsu
- **Related:**
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams public-preview message reactions](teams-message-reactions-preview.md)

---

## Context

The [real-send decision](teams-real-send-acknowledgement.md) returns the Bot
Framework activity ID for every confirmed Teams send.
That ID is required for Bot Connector update and delete APIs, but accepting an
arbitrary activity ID from Core would allow OpenAB to attempt mutation of an
inbound user message or a bot message from another tenant or conversation.

The existing `GatewayReply.reply_to` field is overloaded by older command
paths: normal sends use it as the origin OpenAB event ID, while edit and delete
commands historically place the platform message target in it. Keeping that
overload for new peers would again risk passing an OpenAB event ID to a Bot
Connector activity URL.

The reliability boundary remains single-process and process-local. It does not
provide durable ownership, cross-replica coordination, or mutation of messages
created before restart.

## Decision

### Additive command target

`openab.gateway.reply.v1` gains an optional `target_message_id` field. The
capability contract gains a fail-closed `supports_target_message_id` flag.

For a negotiated peer that advertises support, Core sends command replies as:

```json
{
  "reply_to": "evt_origin",
  "target_message_id": "platform_activity_id",
  "command": "edit_message"
}
```

`reply_to` remains origin event correlation. `target_message_id` is the
platform activity targeted by edit, delete, or an opt-in reaction command.
Normal sends never interpret `reply_to` as a command target.

Compatibility behavior is:

- new Core + new Gateway: preserve the origin event and send the explicit
  target field;
- new Core + old Gateway, or an operation before hello completes: omit the new
  field and copy the command target into legacy `reply_to`;
- old Core + new Gateway: when the field is absent, treat `reply_to` as the
  legacy command target;
- Unified: use the explicit field for Teams and legacy form for adapters that
  do not advertise support.

Missing capability fields default to false. The protocol version remains v1
because both capability and reply fields are additive and covered by
old-peer decoding tests.

### Process-local ownership index

After a Teams create/send returns `Delivered` with a non-empty activity ID, the
Gateway records:

```text
(app_id, tenant_id, conversation_id, activity_id)
    -> authenticated route + ownership_created_at
```

The ownership index:

- is process-local;
- uses `teams.route_ttl_secs` as its lifetime;
- has an independent `teams.max_route_entries` capacity bound;
- evicts its oldest entry at capacity with an operator warning;
- is swept by the existing Teams ingress cleanup task;
- stores the already validated gateway-local service URL and never exposes it
  to Core, the agent, ACK payloads, or logs.

Inbound activity IDs are not inserted. Only a confirmed outbound activity can
be edited or deleted.

A new-field command must still have a live origin event route. The route fixes
the app, tenant, and conversation scope before ownership lookup. A missing or
expired origin returns `target_origin_not_found`; a channel mismatch returns
`target_scope_mismatch`; a target outside that exact scope returns
`message_not_owned`.

A legacy command has no separate origin event. Gateway may use a unique owned
entry matching the configured app, command conversation, and target activity.
If the same legacy tuple exists in more than one tenant, it fails closed with
`target_scope_ambiguous`.

### Bot Connector operations

Owned edits call:

```text
PUT {serviceUrl}/v3/conversations/{conversationId}/activities/{activityId}
```

Owned deletes call:

```text
DELETE {serviceUrl}/v3/conversations/{conversationId}/activities/{activityId}
```

Conversation and activity IDs continue to use URL path-segment encoding and the
commercial-public-cloud endpoint policy. OAuth acquisition, same-origin
redirect policy, 5-second connect timeout, 10-second request timeout, 4 KiB
error body cap, and redaction rules are unchanged.

Mutation outcomes are classified as follows:

- HTTP success: `Delivered` without a required message ID;
- explicit `3xx` or `4xx`: `Rejected`;
- `429`: `Rejected` with parsed `retry_after_ms` unless the bounded internal
  retry succeeds;
- `5xx`, request timeout, disconnect, or transport failure: `Unknown`;
- route, ownership, target, or command validation failure before HTTP:
  `Rejected`.

POST sends are never retried. PUT and DELETE perform at most one internal retry
only after an explicit `429` response proves that attempt was rejected, and
only when `Retry-After` is present and no greater than one second. A second
`429`, a longer delay, a timeout, disconnect, or `5xx` is returned immediately.
Core does not retry a terminal `Rejected` or `Unknown` outcome.

A delivered delete removes the ownership entry. A rejected delete retains it
for a later corrected attempt; an unknown delete retains it because the Gateway
cannot safely infer whether Teams applied the operation. Delivered edits retain
the original ownership timestamp rather than turning active mutation into
unbounded retention.

### Operation-specific acknowledgement

A configured Teams adapter advertises:

```text
send_ack = true
edit_ack = true
delete_ack = true
supports_target_message_id = true
can_edit = true
can_delete = true
streaming_mode = disabled
```

New Standalone peers wait for the existing configured Gateway ACK timeout on
edit and delete. Delivered edit/delete ACKs do not require a message ID.
Rejected and unknown outcomes propagate as errors and are not converted to
fire-and-forget success.

Legacy peers retain their previous missing-ACK semantics. Unified returns the
same outcome directly and overrides native delete instead of falling back to an
edit-to-zero-width operation.

Enabling edit/delete capability does not enable progressive response. Teams
`streaming_mode` remains disabled; progressive response owns its policy and
lifecycle separately.

### Same-conversation ordering

Every Teams send, edit, and delete acquires a fixed process-local write shard
computed from tenant and conversation ID. There are 64 shards:

- writes in the same tenant/conversation are serialized;
- different conversations normally proceed independently;
- a hash collision may conservatively serialize unrelated conversations;
- the fixed array prevents an attacker or busy tenant from growing a lock map
  without bound.

Ownership and route state are revalidated after lock acquisition. This prevents
a queued edit from running after a preceding delete removed ownership.

## Compatibility matrix

| Core | Gateway | Command targeting and ACK behavior |
| --- | --- | --- |
| old | old | Legacy `reply_to` wire shape; Teams edit/delete remain unsupported and fail closed in a legacy Gateway without bot-owned mutation support. |
| old | new | Legacy target fallback is accepted only if it resolves to a unique bot-owned activity; missing ACK remains non-fatal to old Core. |
| new | old | No target-field capability is advertised, so Core copies the target into legacy `reply_to`; missing required ACK is not enabled. |
| new | new | Origin and target remain separate; ownership is enforced; edit/delete receive operation-specific structured ACKs. |
| Unified | embedded | Teams uses the explicit target and returns the structured mutation outcome directly. |

## Security and reliability boundaries

- Inbound user activities cannot enter the ownership index and therefore cannot
  be edited or deleted.
- New-field mutation cannot cross app, tenant, or conversation scope.
- Ambiguous legacy scope fails closed.
- Service URLs and credentials remain Gateway-local.
- Ownership disappears on restart and is not shared across replicas.
- A successful write ACK is not a durable event log or exactly-once guarantee.
- External deletion, app uninstall, or platform-side retention can make a
  locally owned ID invalid; the Connector response remains authoritative.
- This decision does not add proactive references, persistent ownership, or
  multi-consumer coordination.


## Consequences

### Positive

- OpenAB can update and delete only activities it confirmed creating.
- Event correlation and platform command targets are no longer overloaded for
  new peers.
- Edit/delete uncertainty is explicit and cannot trigger blind fresh sends.
- Same-conversation writes cannot overtake one another within a process.
- Rolling upgrades remain non-lockstep.

### Negative

- Restart, TTL expiry, or capacity eviction makes older bot messages immutable
  through OpenAB.
- New-field commands require their origin route to remain live even if ownership
  was recorded slightly later.
- Fixed lock-shard collisions can reduce concurrency.
- The one-second `429` retry bound may return a retryable rejection to Core
  instead of waiting for a longer platform delay.
- Connector mutation behavior and availability remain controlled by Microsoft.

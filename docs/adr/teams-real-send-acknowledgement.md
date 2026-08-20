# ADR: Teams Real Send Acknowledgement and Reply Correlation

- **Status:** Proposed
- **Date:** 2026-08-08
- **Author:** @NeoHsu
- **Related:**
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Custom Gateway](custom-gateway.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)

---

## Context

The [ephemeral-ingress decision](teams-ephemeral-ingress-state.md) introduced an
authenticated, bounded, gateway-local route for each Teams message activity. Before this decision, outbound Teams delivery still used a
conversation-only service URL cache, copied the OpenAB `event_id` into Bot
Connector `replyToId`, and discarded the activity ID returned by Teams in
Unified mode. Standalone Core could not require a send acknowledgement because
the Teams capability advertised `send_ack = false`.

Those behaviors violated the identifier contract:

- `GatewayEvent.event_id` is OpenAB correlation metadata, not a Bot Framework
  activity ID;
- a successful create/send must return the real platform activity ID;
- a timed-out or disconnected POST may already have completed and must not be
  represented as a safe rejection or retried blindly.

Teams controls channel-root, channel-reply, Personal, and group-chat
presentation; HTTP transport tests cannot define or guarantee that UX.

## Decision

### Route resolution

A commandless outbound reply resolves `GatewayReply.reply_to` exclusively as an
OpenAB `event_id` in the bounded ingress registry. The resolved route supplies:

- bot app and tenant scope;
- conversation ID and type;
- inbound activity and reply-chain identifiers;
- the validated gateway-local service URL;
- optional Team and channel identifiers.

The outbound `GatewayReply.channel.id` must equal the route's conversation ID.
A missing, expired, or capacity-evicted event route returns
`route_not_found`; a conversation mismatch returns `route_mismatch`. Both are
`Rejected` outcomes and occur before OAuth or Bot Connector I/O.

The conversation-only compatibility service URL map is removed. Service URLs
remain inside authenticated route state and are never sent to Core or the
agent.

### Normal send and explicit quote

Normal responses do not infer a Bot Connector `replyToId` from `event_id` or
from the triggering inbound activity. They use the Bot Connector
`SendToConversation` endpoint:

```text
POST {serviceUrl}/v3/conversations/{conversationId}/activities
```

Only `GatewayReply.quote_message_id` expresses an explicit quote request. The
adapter uses the Bot Connector `ReplyToActivity` endpoint and also carries the
target as `Activity.replyToId`, but only when the target is known in the same
app, tenant, and conversation scope:

```text
POST {serviceUrl}/v3/conversations/{conversationId}/activities/{activityId}
```

Known targets include:

- the current authenticated inbound activity;
- its authenticated `replyToId` reply-chain root;
- another activity still present in the same bounded ingress route index.

An empty or unknown target falls back to a plain send. It is never looked up in
another tenant or conversation and never causes `event_id` to reach a Bot
Connector activity URL or body field. This endpoint distinction follows
Microsoft's [Bot Connector API reference](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference?view=azure-bot-service-4.0#reply-to-activity),
which directs replies to a specific activity through `ReplyToActivity` rather
than `SendToConversation`.

### Structured send outcomes

Teams sends produce one of the existing protocol outcomes:

| Condition | Outcome | Code / data |
| --- | --- | --- |
| HTTP success with non-empty activity ID | `Delivered` | real `message_id` |
| Invalid or missing route | `Rejected` | `invalid_route`, `route_not_found`, or `route_mismatch` |
| OAuth acquisition fails before Connector POST | `Rejected` | `connector_auth_failed` |
| Connector `3xx` or `4xx` response | `Rejected` | stable rejection code |
| Connector `429` | `Rejected` | `rate_limited` plus bounded `retry_after_ms` |
| Connector `5xx` | `Unknown` | `connector_server_error` |
| POST timeout, disconnect, or transport error | `Unknown` | `request_timeout` or `transport_error` |
| Success response is malformed or omits activity ID | `Unknown` | `invalid_success_response` or `missing_activity_id` |

A success response without a usable activity ID is `Unknown`, not `Rejected`,
because Teams may already have created the message. OpenAB does not
fresh-send or automatically retry `Rejected` or `Unknown` results on this path.
Error body size, redaction, endpoint validation, redirect policy, and request
timeouts remain governed by the Bot Connector transport-hardening decision.

`Retry-After` accepts either delta seconds or an HTTP date and is converted to a
bounded millisecond value. Recording it does not introduce an automatic retry.

### Standalone acknowledgement

A configured Teams adapter now advertises:

```text
send_ack = true
edit_ack = false
delete_ack = false
```

After a valid new↔new hello negotiation, Core includes a request ID and waits for
the operation-specific send ACK. Gateway emits
`openab.gateway.response.v1` with additive structured outcome fields on every
terminal commandless-send path. `Delivered` must contain a non-empty real
Bot Framework activity ID; Core rejects an otherwise successful ACK without
one.

A legacy Core may omit the request ID or include one for its existing
best-effort streaming correlation. New Gateway emits no unsolicited frame when
the ID is absent. When it is present, the response keeps the legacy fields and
adds outcome metadata that old peers can ignore; a missing response remains
non-fatal under legacy Core semantics. New Core connected to an old Gateway
receives no supported hello and likewise retains legacy missing-ACK behavior.

### Unified acknowledgement

Unified mode calls the same Teams route and Connector implementation in
process. `ChatAdapter::send_message` and `send_message_with_reply` return a
`MessageRef` containing the real Bot Framework activity ID. `Rejected` and
`Unknown` outcomes become errors; Unified no longer fabricates a synthetic ID
for Teams delivery.

Other Unified platforms retain their existing behavior. At this decision's
boundary, Teams capabilities report required send acknowledgement while edit,
delete, streaming, and status remain disabled. The later
[bot-owned mutation decision](teams-owned-message-mutations.md) enables guarded
edit/delete without enabling streaming.

### Activity DTO

The inbound DTO parses the route and presentation fields, including
`activity.id`, `activity.replyToId`, `conversation.id`, `conversation.conversationType`,
`channelData.team.id`, `channelData.channel.id`, and `recipient.id`.
Parsing these fields does not define or guarantee Teams presentation behavior.

## Compatibility

| Core | Gateway | Send behavior |
| --- | --- | --- |
| old | old | Existing legacy fire-and-forget behavior. |
| old | new | Event route is used; no request ID means no response frame, while a legacy request ID receives a backward-compatible response. Missing ACK remains non-fatal to old Core. |
| new | old | No valid hello; Core keeps legacy missing-ACK semantics. |
| new | new | Teams advertises required send ACK and returns a structured terminal outcome with the real activity ID on delivery. |
| Unified | embedded | The same outcome is returned directly without a WebSocket ACK frame. |

Operations emitted before a valid hello is processed continue to use legacy
Core semantics. The Gateway still classifies the internal result, but does not
send an unsolicited response when the reply has no request ID.

## Security and reliability boundaries

- Route lookup is process-local, bounded, and TTL-limited.
- Service URLs never cross the Gateway boundary or appear in outcome messages.
- Quote targets cannot cross app, tenant, or conversation scope.
- Unsupported commands are rejected before route lookup and network I/O;
  reactions remain an intentional no-op until a Teams status backend exists.
- A Gateway broadcast ACK is not a durable record and does not provide crash
  replay.
- This decision does not claim exactly-once delivery, multi-consumer work
  distribution, proactive-send support, or restart-persistent message
  ownership.


## Consequences

### Positive

- OpenAB correlation IDs can no longer leak into Bot Connector activity fields.
- New Standalone and Unified sends expose the real Teams activity ID.
- Rejection and ambiguous delivery are distinct, preventing blind duplicate
  sends after POST uncertainty.
- The temporary conversation-only service URL cache is eliminated.
- Explicit quote targets fail safely to plain send when route evidence is
  missing.

### Negative

- Replies fail after route expiry, capacity eviction, or process restart.
- A successful Teams write with a malformed response is reported as unknown
  even though a message may be visible to the user.
- Teams clients control scope-specific thread and quote presentation; successful
  transport correlation does not guarantee visible quote chrome.
- Required ACKs make Connector latency visible to new Core peers and consume
  the configured 12-second Gateway ACK budget.

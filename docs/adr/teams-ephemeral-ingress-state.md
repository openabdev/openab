# ADR: Teams Ephemeral Ingress Route and Duplicate Suppression

- **Status:** Proposed
- **Date:** 2026-08-07
- **Author:** @NeoHsu
- **Related:**
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Custom Gateway](custom-gateway.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)

---

## Context

Bot Framework may retry the same webhook activity. Before this decision, the
Teams adapter published every authenticated retry, keyed reply routing only by
conversation ID, accepted messages with missing routing identifiers, and
ignored the result of the local Gateway broadcast. An HTTP 200 could therefore
mean that no Core consumer received the event.

The proposed delivery boundary remains deliberately narrow:

- one Gateway process replica;
- one supported Standalone Core consumer;
- process-local state only;
- no crash replay, durable inbox, shared idempotency store, or exactly-once
  claim.

## Decision

### Required message fields

A message activity proceeds only when it contains non-empty values for:

- Bot Framework channel ID;
- tenant ID;
- conversation ID;
- activity ID;
- sender ID;
- service URL.

Structural presence is checked before JWT key lookup; route and dedupe state are
created only after JWT and tenant authorization. The service URL must also pass
the Microsoft commercial public-cloud endpoint policy. Invalid or missing
fields return HTTP 400 and do not create route or dedupe state. Non-message and
structurally valid empty-text activities retain their existing HTTP 200 ignore
behavior.

The adapter also parses optional Bot Framework `replyToId`, Team ID, and channel
ID into gateway-local route state. These values are not sent to the agent.

### Composite identity

Both route correlation and duplicate suppression use the composite identity:

```text
(app_id, tenant_id, conversation_id, activity_id)
```

This prevents an activity ID collision from crossing applications, tenants, or
conversations. A generated `GatewayEvent.event_id` is a separate correlation
index for the future outbound route lookup. It is never a Bot Framework
activity ID.

### Publication state machine

Each composite key follows:

```text
Vacant -> Publishing -> Accepted
             |
             +-> local publish failure -> Vacant
```

The first request owns publication. A concurrent duplicate that observes
`Publishing` waits on the same in-process completion signal. It returns HTTP
200 only if the owner reaches `Accepted`; otherwise it returns HTTP 503.

An `Accepted` duplicate returns HTTP 200 without publishing another
`GatewayEvent`. A failed local broadcast removes the publishing entry before
returning HTTP 503, so a later Bot Framework retry may publish again.

The Gateway checks the result of `broadcast::Sender::send` rather than a
separate receiver-count preflight, avoiding a check-then-send race. A successful
local broadcast is the process-local acknowledgement boundary; it does not prove ACP or
outbound completion.

### Bounded process-local state

Three positive settings control state:

| Setting | Default | Purpose |
| --- | ---: | --- |
| `teams.dedupe_ttl_secs` | 600 | Accepted duplicate suppression window |
| `teams.route_ttl_secs` | 3600 | Authenticated ephemeral route lifetime |
| `teams.max_route_entries` | 10000 | Independent capacity bound for route and dedupe maps; bot-owned mutation state uses the same independent bound |

Equivalent Standalone environment variables are
`TEAMS_DEDUPE_TTL_SECS`, `TEAMS_ROUTE_TTL_SECS`, and
`TEAMS_MAX_ROUTE_ENTRIES`.

Expired entries are removed during reservation and by a shared background
sweeper used in both Standalone and Unified mode. At capacity, the oldest
accepted dedupe entry or route may be evicted with a warning. Active
`Publishing` entries are never evicted to admit another key; saturation returns
HTTP 503. A stale publishing owner is failed after a bounded internal timeout so
waiters cannot remain blocked indefinitely.

The initial route implementation retained the legacy conversation-to-service-URL
cache for the existing outbound path. The real-send implementation removed that
compatibility cache: outbound sends
now resolve the authenticated route directly by `event_id` and verify the
reply's conversation before using its gateway-local service URL.

## Security and privacy

- Service URLs remain gateway-local and are excluded from Gateway wire events,
  agent prompts, response payloads, and logs.
- Endpoint validation happens before route persistence.
- Logs may include tenant, conversation, sender, and validated service host,
  but never the full service URL or app secret.
- Route state is not promoted to a proactive conversation reference.
- Duplicate suppression occurs only after JWT and tenant checks; unauthenticated
  input cannot poison the cache.

## Compatibility

This is a correctness change for malformed or undeliverable Teams webhooks:

| Condition | Previous behavior | New behavior |
| --- | --- | --- |
| Missing required route field | Often HTTP 200 | HTTP 400 |
| Accepted duplicate | Published again | HTTP 200 without republish |
| No local event consumer | HTTP 200 | HTTP 503, no tombstone |
| Local publish succeeds | HTTP 200 | HTTP 200 and route accepted |

No Gateway wire field is removed or made mandatory. Unified and Standalone use
the same adapter state machine. Existing configuration remains valid through
the documented defaults.


## Consequences

- Duplicate suppression does not survive restart and does not span replicas.
- Capacity eviction may shorten the effective dedupe window under sustained
  overload; warnings make this visible.
- A successful broadcast can still be lost after process failure or consumer
  lag. Durable delivery requires a later inbox/outbox design.
- [Real send acknowledgement](teams-real-send-acknowledgement.md) uses this
  route for real activity IDs and outbound correlation.

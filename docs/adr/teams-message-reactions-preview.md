# ADR: Teams Public-Preview Message Reactions

- **Status:** Proposed
- **Date:** 2026-08-09
- **Author:** @NeoHsu
- **Related:**
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)

---

## Context

Microsoft's Teams SDK exposes public-preview add/remove reaction operations on
the Bot Connector conversation API. They use the authenticated Bot Framework
`serviceUrl`, conversation ID, activity ID, and bot token already required for
normal sends. They do not require Microsoft Graph, RSC, a delegated user token,
or a new manifest permission.

OpenAB previously treated `add_reaction` and `remove_reaction` as successful
no-ops for Teams. Enabling the preview by default would change existing
behavior, create new status side effects, and rely on a tenant feature that is
not yet generally available.

## Decision

### Explicit opt-in

Add this first-class setting:

```toml
[teams]
reactions_enabled = false
```

The environment fallback is `TEAMS_REACTIONS_ENABLED`. Only `true` or `1`
enables the preview. Missing, false, zero, empty, or invalid values remain
fail-closed at `false`.

When disabled:

- reaction commands preserve the legacy successful no-op;
- no route lookup, token request, or Connector write occurs;
- Teams advertises `status_backend = none` to a negotiated Core.

When enabled, Standalone and Unified advertise `supports_reactions = true`.
They select `status_backend = reactions` unless Core explicitly selects a
separate processing-message backend. In that combined mode, reactions provide
only permanent queued receipts while a turn-local message provides transient
progress. Streaming remains disabled and reaction support does not enable any
other Teams capability.

### Bot Connector operations

OpenAB uses the existing Bot Framework token and validated public-cloud
`serviceUrl`:

```text
PUT    {serviceUrl}/v3/conversations/{conversationId}/activities/{activityId}/reactions/{reactionType}
DELETE {serviceUrl}/v3/conversations/{conversationId}/activities/{activityId}/reactions/{reactionType}
```

All path values are appended as URL path segments. Empty reaction writes send
`Content-Length: 0`; the Connector otherwise rejects PUT requests that have
neither content length nor chunked framing. The
existing same-origin redirect policy, timeout, bounded error body, token
redaction, and commercial public-cloud endpoint policy apply unchanged.

Reaction writes share the idempotent PUT/DELETE outcome classifier. HTTP
success is `Delivered`; explicit 3xx/4xx is `Rejected`; timeout, disconnect, or
5xx is `Unknown`. An explicit `429` with `Retry-After` no greater than one
second receives at most one internal retry. A bounded retry emits a warning
containing only the static operation name and `retry_after_ms`; it does not log
the Connector URL, conversation, activity ID, or token. There is no
POST/fresh-send fallback.

### Scope and target trust

A reaction target may be:

- an authenticated inbound activity retained in the process-local route index;
- the authenticated reply-chain root of the command's origin route; or
- a confirmed bot-owned activity in the process-local ownership index.

New-field commands require a live origin event route and cannot cross app,
tenant, or conversation scope. Legacy commands without a separate origin are
accepted only when app, conversation, and activity resolve to one unique route;
cross-tenant ambiguity fails closed.

Reaction writes use the same fixed tenant/conversation write shards as sends,
edits, and deletes, with route state revalidated after lock acquisition.

### Reaction IDs

Core emits Unicode status emoji, while the Connector preview expects Teams
reaction IDs. OpenAB maps all default status and completion emoji to IDs from
the Microsoft Teams reactions reference. The generic controller's hard-coded
soft- and hard-stall states are included: `🥱` maps to
`1f971_yawningface`, while `😨` maps to the distinct `fearful` ID so a later
`😱` / `screamingfear` error swap cannot add and remove the same reaction. A
configured value may also be an ASCII reaction ID containing only letters,
digits, `_`, or `-`, up to 128 bytes. Unknown Unicode and unsafe identifiers
are rejected before HTTP.

### Deliberate exclusions

This preview slice does not:

- process inbound `messageReaction` activities;
- add Graph or RSC permissions;
- add reaction-specific required ACK negotiation;
- promise availability outside Microsoft commercial public cloud;
- make native reactions part of the default processing-indicator contract;
- persist route evidence across restart or replicas.


## Consequences

### Positive

- Operators can test visible Teams reactions without widening Graph authority.
- Existing deployments remain unchanged until explicit opt-in.
- Reactions cannot be aimed at arbitrary activity IDs or leak `serviceUrl`.
- Default OpenAB status emoji work without operator-specific mapping.

### Negative

- Preview availability and rendering remain tenant-dependent.
- Rapid generic status transitions may encounter the platform's reaction rate
  limit; bounded retries do not guarantee every cosmetic transition is shown.
- Inbound user reactions remain ignored until a separate behavior and trust
  contract is approved.

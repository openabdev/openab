# ADR: Teams Typed Scope and Mention Routing

- **Status:** Proposed
- **Date:** 2026-08-07
- **Author:** @NeoHsu
- **Related:**
  - [Multi-platform adapter architecture](multi-platform-adapters.md)
  - [Identity trust-none](identity-trust-none.md)
  - [Teams ephemeral ingress state](teams-ephemeral-ingress-state.md)

---

## Context

Before typed scope was added, Teams published only a conversation route,
sender, raw text, and an empty mention list. Core therefore evaluated every Gateway event with
`is_dm = false`, could not distinguish Personal, group chat, and Team channel
scope, and could not prove that a structured Teams mention targeted the
receiving bot. Pure text could also resemble an `@mention` without carrying an
authenticated mention entity.

This proposal must add scope and mention evidence without changing outbound
routing, widening Graph authority, or requiring a lockstep Core/Gateway rollout.

## Decision

### Additive wire fields

`openab.gateway.event.v1` gains three optional fields. The schema and protocol
version remain unchanged because old decoders ignore unknown fields and new
decoders default absent fields:

```rust
struct GatewayScope {
    tenant_id: Option<String>,
    team_id: Option<String>,
    channel_id: Option<String>,
    conversation_type: String,
    trust_scope_id: String,
    is_dm: bool,
}

struct RecipientInfo {
    id: String,
    name: String,
}

struct MentionInfo {
    id: String,
    text: String,
}

struct GatewayEvent {
    // existing fields remain unchanged
    scope: Option<GatewayScope>,
    recipient: Option<RecipientInfo>,
    mention_entities: Vec<MentionInfo>,
}
```

The existing `mentions` array remains a list of mentioned entity IDs for
cross-platform structural gating. `mention_entities` carries the exact Teams
entity text needed for safe recipient-mention removal. Neither field carries a
service URL, token, or proactive conversation reference.

`ChannelInfo.id` remains the Bot Connector conversation ID used for session and
outbound routing. It must not be replaced by `trust_scope_id`.

### Teams scope derivation

After JWT, tenant, required-route, and public-cloud service URL validation, the
Gateway canonicalizes known Bot Framework conversation types:

| `conversationType` | `is_dm` | Required typed fields | Opaque `trust_scope_id` shape |
| --- | --- | --- | --- |
| `personal` | `true` | tenant + conversation | `teams:{tenant}:personal:{conversation}` |
| `groupChat` | `false` | tenant + conversation | `teams:{tenant}:group-chat:{conversation}` |
| `channel` | `false` | tenant + Team + channel | `teams:{tenant}:team:{team}:channel:{channel}` |

The key is opaque and is never parsed for authorization. Raw Team and channel
IDs remain separate fields for allowlist matching. Unknown conversation types,
`is_dm` contradictions, empty `trust_scope_id`, or channel scope missing Team or
channel IDs fail closed in the new Core.

### Scope policy and compatibility

First-class Teams scope settings are:

```toml
[teams]
allowed_teams = []
allowed_channels = []
allow_personal = true
allow_group_chats = true
```

Rules:

- Personal is admitted only when `allow_personal = true`.
- Group chat is admitted only when `allow_group_chats = true`.
- For Team channels, both lists empty means L2 open. If either list is non-empty,
  a Team ID or channel ID match admits the scope.
- L3 `allowed_users` remains an independent security gate and is never bypassed
  by an L2 scope match.

Presence of any new Teams scope field or corresponding environment variable
opts into typed policy. If none is present, Core preserves the existing generic
Gateway L2 behavior: `GATEWAY_ALLOWED_CHANNELS` and `[gateway].allowed_channels`
continue matching `ChannelInfo.id` (the conversation ID). This legacy fallback
is explicit and observable; it prevents a rolling upgrade from silently
opening or closing an existing deployment. New Gateway events without a new
Core are ignored additively. New Core receiving an old event without `scope`
uses the legacy gate.

### Mention trust and trigger matrix

The Gateway parses only `Activity.entities[]` entries whose type is `mention`
and whose `mentioned.id` is non-empty. It publishes their IDs in `mentions` and
their ID/text pairs in `mention_entities`. `Activity.recipient.id` is published
separately.

New↔new trigger behavior is:

| Scope | Trigger |
| --- | --- |
| Personal | Every otherwise trusted user message; mention not required |
| Group chat | A structured mention entity has `mentioned.id == recipient.id` |
| Team channel root/reply | A structured mention entity has `mentioned.id == recipient.id` |
| Unknown/malformed scope | Drop before commands, sessions, reactions, or ACP |

Pure text such as `@OpenAB` or `<at>OpenAB</at>` without a matching entity never
satisfies group/channel mention gating. Thread presence does not bypass Teams
mention gating; ambient/RSC reading remains a separate feature.

After structural mention and L2/L3 trust gates allow the event, Core removes
only entity text associated with the recipient bot ID. It maps entity order to
text occurrences, removes matched recipient ranges in reverse order, and trims
only the resulting edges. Other user/bot mentions and arbitrary whitespace are
preserved. A malformed recipient entity may trigger by ID but is not removed
unless its exact non-empty text occurs. Mention-only text is ignored when no
attachment blocks remain.

Core sets `SenderContext.receiver_id` from `recipient.id`; sender identity,
conversation routing, event correlation, and message ID semantics remain
unchanged.

## Security and reliability boundaries

- Scope and mention evidence is created only after existing Bot Framework JWT,
  tenant, and service URL validation.
- Mention markup is not authority; structured entity IDs are.
- Scope allowlists do not replace L3 user trust.
- This decision adds no Graph, RSC, delegated token, manifest permission, ambient
  reading, attachment download, or persistent conversation state.
- Service URLs and credentials remain Gateway-local.
- Standalone and Unified paths use the same Core scope, mention, and prompt
  normalization helpers.

## Acceptance criteria

Automated tests must cover:

- additive new/old wire decoding;
- Personal, groupChat, and channel scope derivation;
- missing Team/channel fields and unknown conversation types;
- typed Team-or-channel allowlist semantics and legacy conversation fallback;
- correct `is_dm` and L3 identity ordering;
- genuine recipient mention, pure-text spoof, reply mention, multi-mention,
  duplicate mention, and malformed entity cases;
- removal of only the recipient mention while preserving other text;
- slash-command recognition after recipient mention removal;
- Standalone and Unified use of the same helpers;
- config, environment fallback, Helm, and platform-schema conformance.

## Consequences

### Positive

- Core receives an explicit, authenticated Teams scope instead of guessing from
  a route ID.
- Structured mention gating prevents textual mention spoofing.
- Personal behavior remains mention-free and existing generic L2 restrictions
  retain a non-lockstep fallback.
- Agent context can identify the receiving bot in multi-agent deployments.

### Negative

- Typed Teams L2 policy requires additional Core configuration and tests.
- Old Core cannot enforce new group/channel mention semantics until upgraded.
- Mention text lacks explicit character offsets, so cleanup relies on entity
  order plus exact text matching and deliberately leaves unmatched markup.

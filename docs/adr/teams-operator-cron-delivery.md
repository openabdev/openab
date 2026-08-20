# ADR: Teams Operator Cron over Trusted Persistent Conversations

- **Status:** Proposed
- **Date:** 2026-08-20
- **Author:** @NeoHsu
- **Related:**
  - [Teams trusted persistent conversation registry](teams-trusted-persistent-conversation-registry.md)
  - [Gateway capability and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Basic cron scheduler](basic-cronjob.md)

---

## Context

OpenAB's scheduler can dispatch operator-configured prompts through Discord,
Slack, Telegram, Google Chat, and LINE WORKS adapters. It first posts a visible
cron trigger, then starts or reuses the ACP session only after that trigger send
succeeds. Teams is intentionally absent from `VALID_PLATFORMS` and from the cron
adapter map because ordinary Teams sends require a short-lived inbound event
route.

The [persistent-registry proposal](teams-trusted-persistent-conversation-registry.md)
adds an explicit-path, default-off Gateway-local registry. A route enters that
registry only after Bot Framework authentication plus Core structural,
typed L2, and L3 admission. The registry retains the complete
app/tenant/Bot-Framework-channel/conversation identity and validated
`serviceUrl`, and exposes only active, non-expired records for
operator-scheduled delivery. Core and ACP never receive the stored reference or `serviceUrl`.

This proposal connects the existing operator scheduler to that registry without
turning agent-writable usercron into cross-conversation authority. It does not add a new
scheduler, create Teams conversations, install an app, or implement `/remind`.

## Constraints

- Existing deployments and non-Teams cron behavior remain unchanged by default.
- A Teams cron job may target only an exact active, non-expired registry record.
- The complete registry identity must be enforced; conversation ID alone is not
  a registry key.
- `serviceUrl`, the stored record, and app credentials remain Gateway-local and
  never enter Core config, the Gateway wire, ACP input, responses, or logs.
- Config-defined baseline cron is operator authority. The hot-reloaded
  `cronjob.toml` is explicitly agent-writable in current OpenAB documentation and
  is not equivalent authority.
- Standalone and Unified must apply the same lookup, send, delivery-outcome, and
  registry-reconciliation rules.
- Every accepted content POST has a real Bot Framework activity ID. Ambiguous
  writes are `Unknown` and are never retried blindly.
- The baseline remains one Gateway writer and one active Standalone Core consumer.

## Current-System Findings

### Scheduler authority and ordering

`[[cron.jobs]]` is immutable process config and is validated at startup. The
scheduler sends the visible cron trigger before it calls `AdapterRouter`, so a
failed destination consumes no ACP session or agent work. The scheduler also
prevents overlap for one job and treats the next matching schedule as a new
execution rather than retrying a failed tick.

The separate `cronjob.toml` overlay is disabled by default but may be
hot-reloaded and written by the agent. It can also execute an operator-approved
local `disable_on_success` command. It therefore cannot safely select an
arbitrary trusted Teams record without a future user/scope binding design.

### Standalone adapter lifetime

At the design baseline, Unified mode retained one shared adapter for cron.
Standalone constructed a new `GatewayAdapter` inside each WebSocket connection
loop and did not expose it to the scheduler. This proposal needs a stable
reconnect-aware proxy: the
proxy delegates to the current negotiated connection, clears it on disconnect,
and never creates a second Gateway WebSocket consumer.

### Microsoft Teams proactive delivery

Microsoft documents scheduled messages as proactive messages. The app must
already be installed in the destination; a stored conversation ID or
conversation reference is required, and the incoming `serviceUrl` should be
retained instead of hardcoding an endpoint. Proactive messaging cannot create a
new group chat or a new Team channel. A blocked or uninstalled app can return
HTTP 403 with `MessageWritesBlocked`; Microsoft also documents
`BotNotInConversationRoster` when the bot is no longer a member of the
conversation.

### Reviewed prior art

OpenClaw revision
[`4994f7bacf308269a0770b4a912c44a746cccec7`](https://github.com/openclaw/openclaw/tree/4994f7bacf308269a0770b4a912c44a746cccec7)
resolves an outbound target through its conversation store, validates the
stored service endpoint/cloud boundary, reconstructs the SDK reference inside
the Teams plugin, and sends directly to the stored conversation. OpenAB adopts
the Gateway-local resolution and endpoint validation, but requires the complete
registry key and active state rather than a conversation-ID-only lookup.

Hermes Agent revision
[`00e5a361b60f621ae2246dffdd0a0252895d8493`](https://github.com/NousResearch/hermes-agent/tree/00e5a361b60f621ae2246dffdd0a0252895d8493)
uses an operator-configured standalone Teams destination and validates its
service host and conversation identifier. OpenAB adopts explicit operator
selection but not a static service URL in Core or config; the trusted registry
remains the only source of the endpoint.

## Decision

### 1. Authority boundary

Only baseline `[[cron.jobs]]` entries may set `platform = "teams"` under this proposal.
These entries are operator-controlled process configuration and intentionally
bypass inbound user L2/L3 checks at execution time, as existing operator cron
does. Their destination is still constrained to a record that previously
passed those checks and remains active.

The usercron loader rejects every Teams entry with a bounded, identifier-free
warning. It does not partially execute the job, resolve a registry record, send
a message, or start ACP. `/remind`, agent-created recurring schedules, and any
user-facing target selector remain separate follow-ups that must bind the
initiating user and scope and revalidate them at execution.

### 2. Additive target configuration

A Teams baseline job keeps `channel` as the stored Teams conversation ID and
adds one required field:

```toml
[[cron.jobs]]
enabled = true
schedule = "0 9 * * 1-5"
platform = "teams"
channel = "<conversation-id>"
teams_tenant_id = "<tenant-id>"
message = "summarize yesterday's merged work"
sender_name = "DailyOps"
timezone = "Asia/Taipei"
```

For Teams jobs:

- `teams_tenant_id` must be present, non-empty, and bounded;
- `thread_id` is rejected because the trusted conversation already encodes the
  Teams Personal, groupChat, or channel route;
- fields used only by agent-writable usercron remain rejected in baseline config;
- a Teams-specific field on any other platform is rejected as a configuration
  error; and
- `bot_framework_channel_id` is the verified Teams transport constant
  `msteams`, while `app_id` comes from the configured Gateway credential. Neither
  is operator-selectable in Core.

Gateway therefore reconstructs and validates the complete registry key:

```text
(configured_app_id, teams_tenant_id, "msteams", channel)
```

The raw `configToml`/`configUrl` chart contract remains authoritative; this proposal does
not restore removed Helm field rendering or create a second cron configuration
surface.

### 3. Platform-neutral Core route marker

`ChannelRef` gains an optional bounded persistent-conversation target carrying
only tenant ID, Bot Framework channel ID, and conversation ID. It participates
in routing equality so two distinct persistent targets cannot alias, but the
existing session key continues to use the Teams conversation route and does not
serialize the target into ACP sender context.

All existing reactive and non-Teams `ChannelRef` values set the field to
`None`. A Teams cron `ChannelRef` sets it before the visible trigger send; clones
and returned `MessageRef` values preserve it for agent response chunks and
turn-local bot-owned mutations.

### 4. Additive Gateway capability and wire field

`AdapterCapabilities` gains
`supports_persistent_conversation_send`, defaulting to `false`. Gateway
advertises it for Teams only when:

- the trusted persistent registry opened safely;
- Teams send ACK is supported; and
- Standalone reports exactly one active Core consumer.

`openab.gateway.reply.v1` gains an optional closed
`persistent_conversation` object containing the non-secret tenant,
Bot-Framework-channel, and conversation identity. It never contains app secret,
OAuth token, `serviceUrl`, message/activity history, or the serialized registry
record. `reply.channel.id` must equal the target conversation ID and proactive
frames have no inbound `reply_to` correlation.

New Core does not emit this field unless the exact capability is available.
Gateway rejects an unnegotiated field, an unsupported topology, malformed or
mismatched identity, or a non-Teams use before lookup or HTTP. The existing
registration capability is not reused as an optimistic send capability, so
registry-only Gateway peers fail closed.

### 5. Exact Gateway-local lookup

Gateway combines the wire target with its configured app ID and validates all
fields before acquiring the registry lock. It also reapplies the current
Gateway tenant allowlist. It then obtains a clone only if the exact record is
active and non-expired.

Missing, expired, disabled, revoked, cross-tenant, cross-conversation, wrong
Bot-Framework-channel, unsafe-endpoint, unavailable-registry, and ambiguous
records all return a bounded `Rejected` outcome without OAuth, Connector HTTP,
filesystem mutation, ACP work, or fallback to the ephemeral route cache.
Expired records are not refreshed by cron; only a new trusted inbound activity
can refresh or reactivate them.

### 6. Delivery and turn ordering

For each Teams execution:

1. the scheduler builds the persistent target from operator config;
2. Gateway performs the exact active lookup and conversation write lock;
3. Gateway rechecks active state after locking;
4. the existing outcome-aware Connector path posts one visible cron trigger;
5. only `Delivered` with a non-empty real activity ID allows session/ACP work;
6. agent response sends and ordered chunks repeat the active lookup using the
   preserved target; and
7. turn-local ownership records permit only the activities created by this
   process to be edited, deleted, or reacted to.

Teams is added to the scheduler's threadless platforms. It never calls
`create_thread` or `rename_thread`, and no operator-cron path creates a new Teams
conversation. Personal, groupChat, and channel behavior comes from the trusted
stored conversation itself.

### 7. Registry reconciliation

After a Connector write, Gateway updates the exact registry key without holding
its filesystem mutex across network I/O:

- `Delivered` clears one prior consecutive forbidden-write count;
- an exact HTTP 403 `MessageWritesBlocked` or
  `BotNotInConversationRoster` increments the count and disables the active
  record on the second consecutive result;
- generic 401/403, 404, 413, 429, other 4xx, 5xx, timeout, disconnect, and every
  `Unknown` outcome leave state unchanged; and
- only a later trusted inbound promotion can reactivate a disabled or revoked
  record.

The 403 classifier parses only bounded structured error bodies and emits a
bounded internal reason code; it does not log or persist the response body or
user identity. A registry reconciliation failure is logged count-only and does
not rewrite the already authoritative Connector outcome. No outcome is retried
inside the cron execution.

### 8. Reconnect-aware Standalone proxy

Main creates one stable `ChatAdapter` proxy before spawning the existing
Standalone Gateway connection loop and registers that proxy for Teams cron.
Each connection generation installs its negotiated concrete adapter; disconnect
clears only that generation and wakes pending requests. The proxy never opens a
socket itself.

A call made while disconnected or before the persistent-send capability is
negotiated is rejected before wire output. A call whose frame was accepted but
whose ACK is lost remains `Unknown`. After reconnect, later independent cron
executions use the new adapter and re-resolve the durable record, while an old
connection cannot clear or satisfy requests owned by the new generation.

### 9. Observability and privacy

Teams cron logs may expose platform, schedule/source class, operation class,
outcome class, elapsed time, aggregate registry state counts, and bounded reason
codes. They must not expose the configured prompt, app/tenant/conversation,
Team/channel/activity/sender IDs, target object, full registry path,
`serviceUrl`, credentials, or Connector response body.

The scheduled prompt necessarily enters the selected ACP session, but the
persistent target does not. Tests and validation records use only synthetic values or sanitized counts,
order, and UI structure.

## Compatibility and Rollout

| Core | Gateway | Behavior |
| --- | --- | --- |
| old | old | Teams cron remains unsupported. |
| old | new | No persistent target is emitted; reactive registry behavior is unchanged. |
| new | registry-only Gateway | Capability defaults false; Teams cron stops before wire/ACP. |
| new | new, registry off | Capability false; Teams cron stops before wire/ACP. |
| new | new, registry on | Exact active lookup, required ACK, and reconciliation are enabled. |

Existing non-Teams cron jobs retain their defaults and routing. A recommended
Standalone rollout is Core first with no imminently firing Teams job, verify the
old Gateway fail-closed path, then upgrade Gateway, verify the negotiated
capability, and only then enable a Teams baseline job. Gateway rollback leaves
the registry file untouched; a new Core paired with the rolled-back Gateway simply
cannot fire Teams cron.

## Acceptance criteria

Automated coverage must include:

1. additive config parsing/defaults plus Teams tenant, field-bound, thread, and
   cross-platform validation;
2. baseline Teams acceptance and unconditional usercron Teams rejection;
3. `VALID_PLATFORMS`, threadless behavior, configured-platform detection, and
   exactly one selected cron adapter in Unified and Standalone modes;
4. stable Standalone proxy behavior before connect, after hello, during ACK,
   after disconnect, and after a new connection generation;
5. additive capability and target wire round trips, missing-field defaults, old
   peers, unsupported topology, and no pre-negotiation send;
6. exact complete-key lookup with current tenant allowlist and no HTTP for
   missing/expired/disabled/revoked/mismatched targets;
7. one real-ID trigger before session/ACP work, no synthetic Teams thread, and
   target preservation through content chunks and turn-local ownership;
8. `Delivered`, `Rejected`, and `Unknown` propagation with no blind retry;
9. exact blocked/not-in-roster parsing, two-result disable, successful reset,
   generic-403/429/5xx/timeout no-state-change, and reconciliation-write failure;
10. same-conversation serialization without cross-conversation head-of-line
    blocking;
11. Unified/Standalone parity and Core-first/new-old rolling combinations;
12. serialization/logging guards proving that persistent records, target IDs,
    response bodies, `serviceUrl`, credentials, and prompts do not leak to ACP,
    responses, or logs; and
13. targeted Core/Gateway/Unified/platform-schema tests, config documentation,
    changed-line formatting, shipping-target Clippy, LSP, links, secret scans,
    and `git diff --check`.


## Consequences

### Positive

- Teams scheduled delivery reuses the trust already proven by PR 11.
- The first platform write gates agent cost and every write retains structured
  delivery semantics.
- Core selects a complete logical target without receiving `serviceUrl` or the
  stored reference.
- Explicit capability negotiation preserves rolling fail-closed behavior.
- Usercron cannot turn agent filesystem access into arbitrary Teams proactive
  authority.

### Negative

- Operators must copy a tenant ID and conversation ID into baseline config.
- `ChannelRef`, capability DTOs, and the mirrored Gateway reply schema gain one
  more additive routing concept.
- Standalone needs a reconnect-aware adapter proxy shared with the scheduler.
- Each proactive write performs a registry lookup and may perform an atomic
  state reconciliation.
- A blocked installation is disabled only after two exact outcomes.

## Alternatives Rejected

1. **Allow `platform = "teams"` in usercron.** The documented agent-writable file
   has no initiating-user/scope binding and would grant arbitrary registry
   selection.
2. **Lookup by conversation ID alone.** PR 11 explicitly requires the complete
   app/tenant/Bot-Framework-channel/conversation identity.
3. **Put `serviceUrl` in `[[cron.jobs]]` or Core.** This bypasses registry state,
   refresh, revocation, endpoint validation, and the established secret boundary.
4. **Reuse `supports_conversation_registry` as send support.** A PR 11 Gateway
   can register but cannot proactively resolve and send; optimistic reuse would
   break rolling compatibility.
5. **Create a second Standalone WebSocket for cron.** Broadcast fan-out would
   create an unsupported second Core consumer and duplicate inbound events.
6. **Create a new Teams conversation at fire time.** It requires different
   authority and platform semantics, cannot create group chats/channels, and
   bypasses the trusted stored route.
7. **Treat every 403 as uninstall evidence.** Authentication/config failures and
   unrelated forbidden operations must not disable a trusted route.
8. **Retry timeout, disconnect, or 5xx automatically.** A POST may already have
   committed, so retry can duplicate a user-visible scheduled activity.
9. **Apply Discord thread creation to Teams.** The persistent conversation
   already represents the Teams routing surface; a synthetic child thread is
   neither portable nor authorized.

## References

- [Microsoft: Send proactive messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/send-proactive-messages)
- [Microsoft: Send and receive targeted messages](https://learn.microsoft.com/en-us/microsoftteams/platform/agents-in-teams/targeted-messages)
- [Microsoft: Conversation events and installation updates](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/subscribe-to-conversation-events)
- [Microsoft: Bot Connector authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication?view=azure-bot-service-4.0)
- [OpenClaw Teams proactive send at reviewed revision](https://github.com/openclaw/openclaw/blob/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src/sdk-proactive.ts)
- [OpenClaw Teams send-context resolution at reviewed revision](https://github.com/openclaw/openclaw/blob/4994f7bacf308269a0770b4a912c44a746cccec7/extensions/msteams/src/send-context.ts)
- [Hermes Teams adapter at reviewed revision](https://github.com/NousResearch/hermes-agent/blob/00e5a361b60f621ae2246dffdd0a0252895d8493/plugins/platforms/teams/adapter.py)

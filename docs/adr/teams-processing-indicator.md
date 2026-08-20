# ADR: Teams Processing-Message Indicator

- **Status:** Proposed
- **Date:** 2026-08-07
- **Author:** @NeoHsu
- **Related:**
  - [Gateway capabilities and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Teams bot-owned message mutations](teams-owned-message-mutations.md)
  - [Teams public-preview message reactions](teams-message-reactions-preview.md)
  - [Teams progressive edit response](teams-progressive-response.md)
  - [Turn-boundary message batching](turn-boundary-batching.md)

---

## Context

Teams has no generally available native status API equivalent to Slack
`assistant.threads.setStatus`. Bot Connector typing activities are transient and
not part of a generally available processing-status guarantee. The implemented
Teams reaction backend is Microsoft public preview, explicitly opt-in, and therefore cannot be the
default processing-indicator contract.

This proposal needs a visible, Graph-free processing lifecycle without enabling
content streaming or weakening the existing route and bot-ownership checks. It must
also preserve the batching contract: every event may keep its permanent queued
receipt when reaction preview is enabled, while only the final event in a batch
anchors transient progress.

## Decision

### Explicit opt-in

Add one first-class Teams setting:

```toml
[teams]
processing_indicator = "off" # off | message
```

The environment fallback is `TEAMS_PROCESSING_INDICATOR`. Missing or malformed
environment values resolve to `off`; malformed TOML enum values fail config
parsing. The default is `off`, preserving existing deployments.

`processing_indicator = "message"` selects a processing **message** lifecycle.
It does not enable streaming, reactions, typing activities, Graph, RSC, or any
new manifest permission.

### Capability selection and rolling upgrades

Core gains an internal `StatusBackend::Message` value. The processing message
uses only the existing commandless send plus bot-owned edit/delete operations;
no new Gateway command or protocol-version bump is introduced.

Standalone Core selects the message backend only after a valid Gateway hello
advertises all required primitives for Teams:

- required send, edit, and delete acknowledgements;
- `supports_target_message_id`;
- bot-owned edit and delete support.

Before a valid hello, or when any required primitive is absent, configured
message status fails closed to `StatusBackend::None`. Final answer delivery is
unaffected. Unified mode selects the backend only when the in-process Teams
adapter provides the same primitives.

Reaction availability and progress-backend choice are independent. Add an
additive `supports_reactions` capability bit. A peer that advertises the older
`status_backend = reactions` shape is normalized to reaction support for
rolling compatibility; old Core ignores the new bit.

When both opt-ins are active:

- `supports_reactions = true` keeps permanent queued receipts on every event in
  the batch; and
- `status_backend = message` drives transient thinking/tool progress only on the
  final event.

When `processing_indicator = "off"`, the existing reaction-preview behavior is
unchanged.

### One turn-local status activity

Each admitted dispatch turn creates one turn-local processing controller. Its
identity is the controller instance plus the real Bot Connector activity ID
returned by the initial status send. The controller is constructed from the
final event's `ChannelRef`, including its authenticated `origin_event_id`, so
successive turns never share a status target.

For a batched turn, Core creates exactly one controller from
`batch.last().trigger_msg`. It never creates one status message per queued
receipt.

The controller emits at most one new status activity:

| Transition | Visible text | Transport |
| --- | --- | --- |
| start / thinking | `⏳ Processing…` | commandless POST |
| tool start | `🛠️ Using <tool>…` | PUT same activity |
| tool done / thinking | `⏳ Processing…` | PUT same activity |
| successful terminal | `✅ Completed` | PUT same activity |
| agent error | `❌ Failed` | PUT same activity |
| hard timeout | `⏱️ Timed out` | PUT same activity |
| final delivery failure | `❌ Delivery failed` | PUT same activity |
| clear after delivered final content | — | DELETE same activity |

Tool labels are broker-generated metadata, not user prompt text. Normalize line
breaks and backticks and cap the rendered label before sending it to Teams.
Duplicate states are no-ops.

### Terminal-before-final ordering

Status and content streaming remain separate lifecycles:

1. mark the processing activity terminal before the first final-content write;
2. deliver every final-content chunk through the normal send path;
3. after complete delivery, delete the terminal status activity;
4. if final delivery is incomplete, change the status to
   `❌ Delivery failed` and leave it visible.

This ordering prevents a failed delete from leaving an apparently active
`Processing…` message. If terminal edit succeeds but delete is rejected or
unknown, the recognizable terminal text remains. Ambiguous POST, PUT, or DELETE
outcomes never trigger a fresh status send or blind retry.

Status writes are cosmetic: a status failure is logged and must not suppress,
duplicate, or fresh-send the final answer.

### Receipt and progress separation

`docs/adr/turn-boundary-batching.md` §6.7 remains authoritative:

- each batch event receives its permanent queued receipt sequentially when the
  adapter explicitly supports reactions and global reactions are enabled;
- the turn-local processing controller anchors only on the final event; and
- the controller never removes queued receipts.

Teams with reaction preview disabled has no native queued-receipt side effect;
it still creates at most one opt-in processing message for the turn.

## Failure boundaries

- Initial status POST `Rejected` or `Unknown`: disable message status for that
  turn; do not fresh-send another status.
- Status PUT `Rejected` or `Unknown`: keep the known activity handle for final
  terminal/delete cleanup; do not blind retry.
- Terminal PUT succeeds but DELETE fails: leave terminal text visible.
- Final answer delivery fails: report the existing delivery error and leave a
  delivery-failed terminal status when possible.
- Gateway/Core restart or ownership/route expiry may prevent terminal cleanup.
  Status state is intentionally process-local, matching existing route and
  ownership boundaries; this proposal does not add crash replay or durable
  cleanup.

## Security boundaries

- L1 tenant/JWT, typed L2 scope, structured mention, and L3 identity gates run
  before status creation.
- The initial POST resolves only through the authenticated origin event route.
- PUT/DELETE target only the returned bot-owned activity ID and reuse existing
  tenant/conversation ownership validation and write serialization.
- Status text contains no prompt, service URL, token, tenant ID, conversation
  ID, activity ID, or attachment URL.
- No Graph, RSC, delegated token, proactive registry, or new permission is
  introduced.

## Acceptance criteria

Automated tests must cover:

- config/TOML/environment resolution and fail-closed defaults;
- additive `supports_reactions` old/new capability decoding;
- configured message status remaining disabled before hello or when one
  required primitive is absent;
- Standalone and Unified selecting identical Teams backend semantics;
- one POST followed only by PUTs against its real returned activity ID;
- thinking, tool, success, error, timeout, delivery-failure, and clear order;
- terminal PUT before final content and DELETE only after complete delivery;
- POST/PUT/DELETE rejection or unknown outcomes without fresh-send fallback;
- one status activity per batch, anchored to the final event;
- permanent queued receipts surviving when reaction preview and processing
  message are both enabled; and
- no status side effect before trust gates pass.

## Consequences

### Positive

- Teams gains a visible processing lifecycle without Graph or preview APIs.
- One real activity ID gives deterministic, turn-local mutation ownership.
- Processing messages and queued reaction receipts can coexist without
  conflating their lifecycles.
- Existing deployments remain unchanged by default.

### Negative

- Each enabled turn adds one POST, several bounded PUTs, and normally one
  DELETE to the conversation write stream.
- A process restart can orphan a processing message because status state is
  process-local and not durable.
- Tool-heavy turns need transition coalescing to avoid cosmetic write pressure.
